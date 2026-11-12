//! The regression gate: re-measure, compare against a stored baseline (or a
//! merge-base worktree with `--against-ref`) with per-metric thresholds,
//! exit non-zero on regression. On failure, emits a callgrind triage
//! artifact when valgrind is available.

mod accept;
mod args;
mod compare;
mod harness;
mod identical;
mod imported;
mod measure;
mod outcome;
mod resolve;
mod rounds;
#[cfg(test)]
mod test_support;
mod thresholds;
mod walltime;

use self::accept::accept_cmd;
use self::args::{GateArgs, ImportArgs, against_ref_in, packages_in, with_package};
use self::compare::{CompareCtx, compare, reuse_hint};
use self::harness::{
    harness_note, harness_reasons, harness_verdict, harness_waiver, triage_harness_note,
};
use self::imported::run_imported;
use self::outcome::{err, errored, errored_unbuilt, ratchet_in, triage};
use self::resolve::{Resolved, assertion_failures, nothing_measured, resolve, write_gate_status};
use crate::config::{self, Group};
use crate::gate_lock;
use crate::gate_save::{bench_packages, configure_save, save_head};
use crate::invoke::{self, CommonArgs};

pub use identical::recording;
pub use thresholds::{ALLOC_THRESHOLD_PCT, INSTRUCTIONS_THRESHOLD_PCT};
pub(crate) use thresholds::{COUNTER_FLAT_ABS, COUNTER_FLAT_PCT};

pub fn run(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("accept") {
        return accept_cmd(&args[1..]);
    }
    let packages = packages_in(args);
    if packages.len() > 1 {
        return run_each(args, &packages);
    }
    let mut g = GateArgs {
        common: CommonArgs::default(),
        baseline: "base".into(),
        ratchet: None,
        against_ref: None,
        deps: false,
        allow_gone: false,
        allow_harness_change: false,
        matrix: "default".into(),
        save_baseline: None,
        reuse_base: true,
        measure_new: true,
        import: ImportArgs::default(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if g.common.try_parse(a, &mut it) {
            continue;
        }
        match a.as_str() {
            "--features-matrix" => match it.next() {
                Some(m) => g.matrix = m.clone(),
                None => return err("--features-matrix needs combos like \"default;full\""),
            },
            "--baseline" => match it.next() {
                Some(n) => {
                    g.baseline = n.clone();
                    g.import.baseline_given = true;
                }
                None => return err("--baseline needs a name"),
            },
            "--ratchet" => match it.next() {
                Some(n) => g.ratchet = Some(n.clone()),
                None => return err("--ratchet needs a baseline name"),
            },
            "--against-ref" => match it.next() {
                Some(r) => g.against_ref = Some(r.clone()),
                None => return err("--against-ref needs a git ref"),
            },
            "--save-baseline" => match it.next() {
                Some(n) => g.save_baseline = Some(n.clone()),
                None => return err("--save-baseline needs a name"),
            },
            "--shard" => match it.next().map(|s| invoke::Shard::parse(s)) {
                Some(Ok(shard)) => g.common.shard = Some(shard),
                Some(Err(e)) => return err(&e),
                None => return err("--shard needs I/N"),
            },
            "--import" | "--reference-import" => {
                let (Some(format), Some(file)) = (it.next(), it.next()) else {
                    return err(&format!("{a} needs a format and a file"));
                };
                let side = Some((format.clone(), file.clone()));
                if a == "--import" {
                    g.import.current = side;
                } else {
                    g.import.reference = side;
                }
            }
            "--prefix" => match it.next() {
                Some(p) => g.import.prefix = Some(p.clone()),
                None => return err("--prefix needs a value"),
            },
            "--walltime" => match it.next() {
                Some(m) if m == "hard" || m == "advisory" => g.import.walltime = Some(m.clone()),
                _ => return err("--walltime needs hard or advisory"),
            },
            "--no-reuse-base" => g.reuse_base = false,
            "--deps" => g.deps = true,
            "--allow-gone" => g.allow_gone = true,
            "--allow-harness-change" => g.allow_harness_change = true,
            other => return err(&format!("unknown gate arg {other:?}")),
        }
    }

    if g.import.current.is_some() {
        return run_imported(&g, &invoke::project_root());
    }
    let saving = g.save_baseline.is_some();
    let configured = configure_save(&mut g.common, saving, bench_packages, |common| {
        config::apply(common, Group::Gate)
    });
    let walltime_advisory = match configured {
        Ok(Ok(applied)) => {
            if !g.import.baseline_given
                && let Some(baseline) = applied.settings.baseline
            {
                g.baseline = baseline;
            }
            applied.gate.walltime_advisory
        }
        Ok(Err(e)) => return errored_unbuilt(&g, &e),
        Err(e) => return err(&e),
    };
    if let Some(shard) = g.common.shard {
        println!("gate: shard {shard}: gating the items it owns");
    }

    let mut regressions = 0u32;
    let mut failing_ids: Vec<String> = Vec::new();

    let resolved = match resolve(&g, walltime_advisory) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => {
            println!(
                "gate: nothing to gate for {}: no bench target at {}",
                g.common.pkg.as_deref().unwrap_or("this package"),
                g.against_ref.as_deref().unwrap_or("?"),
            );
            return 0;
        }
        Err(e) => return errored(&g, None, &e),
    };
    let Resolved {
        reference,
        current,
        head,
        harness,
        exe,
    } = resolved;
    // A gate that measured nothing must never pass: a typo'd --filter, an
    // empty registry, or lost records would otherwise be permanently green.
    // A shard that owns nothing now still fails on any item it owned that is gone.
    if let Some(refusal) = nothing_measured(&current, &g.common, "gate") {
        return errored(&g, exe.as_deref(), &refusal);
    }
    if let Some(b) = &current.gating_backend {
        println!("gate: gating backend = {b}");
    }
    harness_note(&harness);
    let root = invoke::workspace_root().ok();
    let accepted = root
        .as_deref()
        .and_then(|r| gate_lock::read(r).ok())
        .unwrap_or_default();
    let buildcost_mode = g.common.backend.as_deref() == Some("buildcost");
    let ctx = CompareCtx {
        deps_mode: g.deps,
        allow_gone: g.allow_gone,
        filter: g.common.filter.clone(),
        shard: g.common.shard,
        pkg: g.common.pkg.clone(),
        buildcost_mode,
        accepted,
        headroom_pct: None,
        walltime_advisory,
    };
    regressions += compare(
        &reference,
        &current,
        "",
        &ctx,
        &mut failing_ids,
        &mut Vec::new(),
        &mut Vec::new(),
    );

    // Ratchet: also compare against a long-lived baseline (e.g. last release)
    // so ten +4% regressions can't compound past a single gate.
    if let Some(ratchet_name) = &g.ratchet {
        let ratchet = invoke::workspace_root()
            .map_err(|e| e.to_string())
            .and_then(|root| ratchet_in(&root, ratchet_name));
        match ratchet {
            Ok(r) => {
                println!("--- ratchet: {ratchet_name} ---");
                let rctx = CompareCtx {
                    deps_mode: false,
                    ..ctx.clone()
                };
                regressions += compare(
                    &r,
                    &current,
                    "RATCHET ",
                    &rctx,
                    &mut failing_ids,
                    &mut Vec::new(),
                    &mut Vec::new(),
                );
            }
            Err(e) => return errored(&g, exe.as_deref(), &e),
        }
    }

    // Checked claims from the runner. They hold HEAD against its own
    // declared numbers, so the reference harness has no say over them and
    // --allow-harness-change never reaches them.
    let assertion_failures = assertion_failures(&current, &mut failing_ids);

    if harness_waiver(&harness, g.allow_harness_change, regressions) {
        println!(
            "gate: {regressions} regression(s) allowed by --allow-harness-change ({})",
            harness_reasons(&harness)
        );
        regressions = 0;
    }
    let failures = regressions + assertion_failures;
    if let Some(r) = &root {
        triage_harness_note(&r.join(".soothfast").join("triage"), &harness);
    }

    write_gate_status(failures);
    if let Some(name) = &g.save_baseline {
        let saved = root
            .as_deref()
            .ok_or_else(|| "cannot find the workspace root".to_string())
            .and_then(|root| {
                let listed = exe
                    .as_deref()
                    .and_then(|exe| invoke::listed_ids(exe, g.common.filter.as_deref()));
                let listed = listed.as_deref();
                save_head(
                    root,
                    name,
                    &current,
                    &head,
                    &g.common,
                    listed,
                    assertion_failures,
                )
            });
        if let Err(e) = saved {
            return err(&format!("failed to save baseline: {e}"));
        }
    }
    if failures > 0 {
        failing_ids.sort();
        failing_ids.dedup();
        // buildcost pseudo-items have no runnable body to profile.
        failing_ids.retain(|id| !id.starts_with("buildcost::"));
        triage(&g.common, &failing_ids);
        if let Some(hint) = reuse_hint(&reference, current.gating_backend.as_deref(), regressions) {
            println!("{hint}");
        }
        println!(
            "gate: FAILED ({failures} regression(s)){}",
            harness_verdict(&harness, regressions > 0)
        );
        1
    } else {
        println!(
            "gate: passed ({} item(s)){}",
            current.items.len(),
            harness_verdict(&harness, false)
        );
        0
    }
}

/// Gate each package in turn, after building every bench target together.
fn run_each(args: &[String], packages: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        common.try_parse(a, &mut it);
    }
    common.pkg = None;
    // A config error is left to each package's leg, which reports it and
    // marks the package gate-failed in its baseline.
    if config::apply_to(&mut common, Group::Gate, packages).is_ok() && common.features.is_none() {
        // `--features` names one package's features; across several cargo
        // wants `pkg/feature`, so leave those builds to the per-package legs.
        prebuild_both_sides(packages, &common, args);
    }

    let mut worst = 0;
    for pkg in packages {
        let rc = run(&with_package(args, pkg));
        if rc != 0 {
            worst = rc;
        }
    }
    worst
}

/// Build every bench target once per side. After a rebase the merge-base
/// side is the expensive one, at release with codegen-units pinned to 1.
fn prebuild_both_sides(packages: &[String], common: &CommonArgs, args: &[String]) {
    let _ = invoke::prebuild_benches(packages, common, None, None);
    let Some(refname) = against_ref_in(args) else {
        return;
    };
    let Ok(target) = invoke::worktree_target_dir() else {
        return;
    };
    let _ = invoke::with_merge_base_worktree(&refname, |wt| {
        invoke::sync_untracked_cargo_config(wt)?;
        // Warming a build cache decides no verdict. The per-package leg
        // syncs this worktree again and its outcome is what gets reported.
        invoke::sync_harness_versions(wt)?;
        crate::seed::seed_worktree(wt, &target);
        let _ = invoke::prebuild_benches(packages, common, Some(wt), Some(&target));
        Ok(())
    });
}
