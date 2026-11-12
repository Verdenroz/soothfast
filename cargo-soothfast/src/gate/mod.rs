//! The regression gate: re-measure, compare against a stored baseline (or a
//! merge-base worktree with `--against-ref`) with per-metric thresholds,
//! exit non-zero on regression. On failure, emits a callgrind triage
//! artifact when valgrind is available.

mod args;
mod compare;
mod harness;
mod identical;
mod resolve;
mod rounds;
#[cfg(test)]
mod test_support;
mod thresholds;
mod walltime;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use serde_json::Value;

use self::args::{
    AcceptArgs, GateArgs, ImportArgs, accept_args, against_ref_in, packages_in, with_package,
};
use self::compare::{CompareCtx, FailedMetric, compare, reuse_hint};
use self::harness::{
    harness_note, harness_of, harness_reasons, harness_verdict, harness_waiver, mark_harness,
    triage_harness_note,
};
use self::identical::{
    StoredFor, base_method_differs, bench_text_digests, identical_head, identical_pass,
    identical_pass_cache,
};
use self::resolve::{
    NewTarget, Resolved, assertion_failures, head_from_cache_or, narrowed, newly_measured,
    nothing_measured, resolve, write_gate_status,
};
use self::rounds::{combine_rounds, round_duration};
use crate::buildstamp;
use crate::config::{self, Config, Group};
use crate::gate_lock;
use crate::gate_save::{HeadRun, bench_packages, configure_save, save_errored, save_head};
use crate::headrun::{
    self, RECOUNT, Settled, cache_head, head_from_runcache, ref_doc, settle, settled_doc,
    store_head, store_identical,
};
use crate::import;
use crate::invoke::{self, CommonArgs, Run};
use crate::itemreuse::{Bin, Reuse};
use crate::workspace;

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

/// Gate an imported run against a baseline or another imported run. Nothing
/// is built or measured, so this works outside a cargo workspace; `root`
/// holds the baselines and `soothfast.toml`.
fn run_imported(g: &GateArgs, root: &std::path::Path) -> i32 {
    let Some((format, file)) = &g.import.current else {
        return err("gate --import needs a format and a file");
    };
    if g.ratchet.is_some() || g.against_ref.is_some() || g.save_baseline.is_some() || g.deps {
        return err("--import gates against --baseline or --reference-import only");
    }
    if g.import.baseline_given && g.import.reference.is_some() {
        return err("--baseline and --reference-import both name the reference; pass one");
    }
    if g.common.shard.is_some() || g.common.pkg.is_some() {
        return err("--import gates a result file whole; --shard and -p do not apply");
    }
    let walltime_advisory = match (
        &g.import.walltime,
        Config::load(root).and_then(|c| c.gate()),
    ) {
        (Some(mode), _) => mode == "advisory",
        (None, Ok(cfg)) => cfg.walltime_advisory,
        (None, Err(e)) => return err(&e),
    };
    let prefix = g.import.prefix.as_deref();
    let (mut current, prefix) = match import::read(format, file, prefix) {
        Ok(read) => read,
        Err(e) => return err(&e),
    };
    if let Err(e) = import::check_prefixes(root, std::slice::from_ref(&prefix)) {
        return err(&e);
    }
    if let Some(f) = &g.common.filter {
        current.items.retain(|id, _| id.contains(f.as_str()));
        if current.items.is_empty() {
            return err("--filter matches no imported item; refusing to gate");
        }
    }
    let reference = match &g.import.reference {
        Some((format, file)) => {
            import::read(format, file, Some(&prefix)).map(|(run, _)| import::to_doc(&run))
        }
        None => match invoke::load_baseline_in(root, &g.baseline) {
            Ok(Some(mut doc)) => {
                // A baseline can hold other prefixes and Rust items; theirs are not missing here.
                if let Some(items) = doc["items"].as_object_mut() {
                    items.retain(|_, v| v["imported"].as_str() == Some(prefix.as_str()));
                }
                Ok(doc)
            }
            Ok(None) => Err(format!("no baseline named {:?}", g.baseline)),
            Err(e) => Err(format!("failed to load baseline: {e}")),
        },
    };
    let reference = match reference {
        Ok(doc) => doc,
        Err(e) => return err(&e),
    };

    let ctx = CompareCtx {
        deps_mode: false,
        allow_gone: g.allow_gone,
        filter: g.common.filter.clone(),
        shard: None,
        pkg: None,
        buildcost_mode: false,
        accepted: gate_lock::read(root).unwrap_or_default(),
        headroom_pct: None,
        walltime_advisory,
    };
    let mut failing_ids = Vec::new();
    let failures = compare(
        &reference,
        &current,
        "",
        &ctx,
        &mut failing_ids,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    if failures > 0 {
        println!("gate: FAILED ({failures} regression(s))");
        1
    } else {
        println!("gate: passed ({} item(s))", current.items.len());
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

/// Record a reviewed delta into `soothfast-gate.lock`, so a future `gate`
/// treats it as the new ceiling for that bench and metric instead of a
/// regression — without touching the threshold for anything else. Only
/// records benches that are failing, or (with `--headroom`) close enough to
/// failing to plausibly be part of the same shift; an unearned entry would
/// let the lock file accumulate speculative headroom instead of a reviewed
/// record of what actually shifted. `--only` names outside that set are
/// skipped, not fatal — see the exit-code contract at the end of this
/// function.
fn accept_cmd(args: &[String]) -> i32 {
    let mut a = match accept_args(args) {
        Ok(a) => a,
        Err(e) => return err(&e),
    };
    let walltime_advisory = match config::apply(&mut a.common, Group::Gate) {
        Ok(applied) => {
            a.baseline = a.baseline.or(applied.settings.baseline);
            applied.gate.walltime_advisory
        }
        Err(e) => return err(&e),
    };
    accept_parsed(a, walltime_advisory)
}

/// `gate accept` once its arguments and config are read.
fn accept_parsed(a: AcceptArgs, walltime_advisory: bool) -> i32 {
    let Some(justification) = a.justification.clone() else {
        return err("gate accept needs --justification \"...\"");
    };

    let g = GateArgs {
        common: a.common,
        baseline: a.baseline.unwrap_or_else(|| "base".into()),
        ratchet: None,
        against_ref: a.against_ref,
        deps: false,
        allow_gone: false,
        allow_harness_change: false,
        matrix: a.matrix,
        save_baseline: None,
        reuse_base: true,
        measure_new: false,
        import: ImportArgs::default(),
    };
    let Resolved {
        reference,
        current,
        harness,
        ..
    } = match resolve(&g, walltime_advisory) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => {
            println!("gate accept: nothing to compare — nothing to accept");
            return 0;
        }
        Err(e) => return err(&e),
    };
    harness_note(&harness);
    if let Some(refusal) = nothing_measured(&current, &g.common, "accept") {
        return err(&refusal);
    }
    let root = match invoke::workspace_root() {
        Ok(r) => r,
        Err(e) => return err(&format!("failed to resolve workspace root: {e}")),
    };
    let mut entries = match gate_lock::read(&root) {
        Ok(e) => e,
        Err(e) => return err(&format!("failed to read {}: {e}", gate_lock::LOCKFILE)),
    };

    let ctx = CompareCtx {
        deps_mode: false,
        allow_gone: false,
        filter: g.common.filter.clone(),
        shard: g.common.shard,
        pkg: g.common.pkg.clone(),
        buildcost_mode: g.common.backend.as_deref() == Some("buildcost"),
        accepted: entries.clone(),
        headroom_pct: a.headroom,
        walltime_advisory,
    };
    let mut candidates = Vec::new();
    let mut near_miss = Vec::new();
    compare(
        &reference,
        &current,
        "",
        &ctx,
        &mut Vec::new(),
        &mut candidates,
        &mut near_miss,
    );
    candidates.extend(near_miss);

    if candidates.is_empty() && a.only.is_empty() {
        println!("gate accept: nothing is failing — nothing to accept");
        return 0;
    }

    let available_ids: BTreeSet<&str> = candidates.iter().map(|f| f.id.as_str()).collect();
    let (selected_ids, skipped): (BTreeSet<&str>, Vec<&str>) = if a.only.is_empty() {
        (available_ids, Vec::new())
    } else {
        let mut selected = BTreeSet::new();
        let mut skipped = Vec::new();
        for id in &a.only {
            if available_ids.contains(id.as_str()) {
                selected.insert(id.as_str());
            } else {
                skipped.push(id.as_str());
            }
        }
        (selected, skipped)
    };
    for id in &skipped {
        println!("WARN: {id} is not currently failing or headroom-eligible — skipped");
    }

    let mut by_id: BTreeMap<&str, Vec<&FailedMetric>> = BTreeMap::new();
    for m in &candidates {
        if selected_ids.contains(m.id.as_str()) {
            by_id.entry(m.id.as_str()).or_default().push(m);
        }
    }
    if by_id.is_empty() {
        return partial(&format!(
            "none of the named benches ({}) are currently failing or headroom-eligible",
            a.only.join(", ")
        ));
    }

    let accepted_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for (id, metrics) in &by_id {
        let entry = entries.entry(id.to_string()).or_default();
        entry.base_fingerprint = reference["items"][*id]["fingerprint"]
            .as_str()
            .unwrap_or("")
            .to_string();
        entry.head_fingerprint = current
            .items
            .get(*id)
            .map(|m| m.fingerprint.clone())
            .unwrap_or_default();
        for m in metrics {
            entry.metrics.insert(m.metric.clone(), m.new_value);
        }
        entry.justifications.push(gate_lock::Justification {
            text: justification.clone(),
            accepted_unix,
        });
        println!(
            "gate accept: {id} — {} metric(s) accepted ({justification:?})",
            metrics.len()
        );
    }

    // Written before the exit code, so a partial `--only` match (exit 3
    // below) still leaves the lock file usable for the next CI step.
    if let Err(e) = gate_lock::write(&root, &entries) {
        return err(&format!("failed to write {}: {e}", gate_lock::LOCKFILE));
    }
    println!(
        "gate accept: recorded {} bench(es) in {}",
        by_id.len(),
        gate_lock::LOCKFILE
    );
    if skipped.is_empty() { 0 } else { 3 }
}

/// Measure the merge-base with HEAD in a temp worktree, interleaving rounds
/// (head, base, head, base) tango-style so slow environmental drift cancels;
/// per-metric minima across each side's rounds form the comparison values.
/// Interleaving is a timing concern: the deterministic gating counters
/// (perfcnt/callgrind) don't drift, so the second round of each side skips
/// them and re-measures only the timing-sensitive backends. Advisory walltime
/// can't fail on that second sample, so it skips the second round.
/// With no such bench target at the merge-base, HEAD alone is measured, as
/// [`newly_measured`] decides, or with `measure_new` false nothing is.
/// `Ok(None)` then means there is nothing to gate. A merge-base already
/// measured under the same conditions is served from the run cache instead
/// of being rebuilt, unless `reuse` is false.
fn measure_ref_interleaved(
    common: &CommonArgs,
    refname: &str,
    reuse: bool,
    walltime_advisory: bool,
    measure_new: bool,
) -> Result<Option<Resolved>, String> {
    const TIMING_ONLY: &[&str] = &["--skip-gating-counters"];
    enum Ref {
        /// The merge-base has no bench target; whether HEAD has one.
        NoBenchTarget(bool),
        /// The one pass, whether it carries the gating counters, and the run
        /// stored for the identical binary with its label.
        IdenticalBinaries {
            run: Result<Run, String>,
            counted: bool,
            stored: Option<StoredFor>,
        },
        /// Reference served by binary identity rather than by commit.
        Cached(Value),
        /// The two full rounds, the timing-only pair unless walltime is
        /// advisory, then what HEAD's and the merge-base's runs may store.
        Rounds(
            Box<[Result<Run, String>; 2]>,
            Option<Box<[Result<Run, String>; 2]>>,
            Settled,
            Settled,
        ),
    }

    // Worktree builds go to a persistent sibling of the parent target dir so
    // dependencies compile once per machine, not once per gate run.
    let wt_target = invoke::worktree_target_dir().ok();
    let measure = |dir: Option<&std::path::Path>, extra: &[&str]| -> Result<Run, String> {
        let td = dir.and(wt_target.as_deref());
        let recs = invoke::run_bench_dir(common, extra, dir, td).map_err(|e| e.to_string())?;
        let mut run = invoke::collect(&recs);
        run.build = Some(buildstamp::capture(
            common.codegen_units_stamp().as_deref(),
            dir,
        ));
        Ok(run)
    };

    let base_sha = invoke::git(&["merge-base", "HEAD", refname])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let head_stamp = buildstamp::capture(common.codegen_units_stamp().as_deref(), None);
    // Naming HEAD's binary costs a no-run build plus an objdump, so it is
    // taken once and threaded to every lookup and store below.
    let (head_dig, runs, head_exe) = headrun::probe("gate", common, &head_stamp);
    // HEAD alone, where the reference is already measured: items whose code
    // a stored measurement covers are spliced and the rest are measured.
    let measure_head_with =
        |exe: Option<&std::path::Path>, unplanned: &[&str]| -> Result<(Run, bool), String> {
            Reuse::measure_head(
                "gate",
                common,
                &runs,
                exe.map(|exe| Bin::head(exe, head_dig.as_deref())),
                recording(),
                unplanned,
                || buildstamp::capture(common.codegen_units_stamp().as_deref(), None),
                |args| measure(None, args),
            )
        };
    let measure_head = |exe: Option<&std::path::Path>| -> Result<Run, String> {
        measure_head_with(exe, &[]).map(|(run, _)| run)
    };
    // Stored regardless of `reuse`: a fresh measurement is worth keeping
    // even for an invocation that declined to trust an existing one.
    let base_known = !base_sha.is_empty();

    if let Some((_, doc)) = runs.reusable_reference(reuse, &base_sha) {
        println!("gate: reusing the measured merge-base {base_sha}");
        let (head, settled, source) = head_from_cache_or(&runs, head_dig.as_deref(), || {
            let head = measure_head(head_exe.as_deref())?;
            let settled = settle("gate", "HEAD's run", &head, || measure(None, RECOUNT).ok());
            Ok((head, settled))
        })?;
        cache_head(&runs, &head, head_dig.as_deref(), &settled);
        return Ok(Some(Resolved {
            harness: harness_of(&doc),
            reference: doc,
            current: head,
            head: source,
            exe: head_exe.clone(),
        }));
    }

    println!("gate: measuring merge-base of {refname} in worktree (interleaved rounds)");
    let mut reused: BTreeSet<String> = BTreeSet::new();
    let pkg = common.pkg.clone();
    let target = common.target.clone().unwrap_or_else(|| "soothfast".into());
    let outcome = invoke::with_merge_base_worktree(refname, |wt| {
        // Only checkable when a package was named; without `-p` cargo picks
        // the default member and there is nothing to look up.
        if let Some(pkg) = &pkg
            && !workspace::has_bench_target(pkg, &target, Some(wt)).map_err(|e| e.to_string())?
        {
            let at_head = measure_new
                && workspace::has_bench_target(pkg, &target, None).map_err(|e| e.to_string())?;
            let none = invoke::HarnessSync::default();
            return Ok((Ref::NoBenchTarget(at_head), None, none));
        }
        // A local .cargo/config.toml (untracked, e.g. a linker pin) wouldn't
        // otherwise reach the worktree and would read as a false mismatch.
        invoke::sync_untracked_cargo_config(wt)?;
        // Both bench binaries must embed the same soothfast harness: a
        // measurement-protocol change between the two locked versions would
        // otherwise be reported as a regression in the measured project.
        // Pinning must precede the base build the binary comparison does.
        let harness = invoke::sync_harness_versions(wt)?;
        if let Some(target) = &wt_target {
            crate::seed::seed_worktree(wt, target);
        }
        let digests = bench_text_digests(common, wt, wt_target.as_deref());
        // Callgrind counts differ by method, and a path dependency on the
        // harness is not pinned, so the sides can disagree.
        let head_cg = runs.env().filter(|e| e.gating_backend == "callgrind");
        let base_env = head_cg
            .zip(digests.as_ref())
            .filter(|(_, d)| d.head != d.base)
            .map(|(_, d)| invoke::host_env(&d.base_exe, common.backend.as_deref()));
        let harness = match head_cg
            .and_then(|head| base_method_differs(head, digests.is_some(), base_env.as_ref()))
        {
            Some(base) => harness.with_method(&base, head_cg.map_or("", |h| h.method())),
            None => harness,
        };
        // A run measured from byte-identical machine code under the same
        // conditions is that binary's measurement, whatever commit built it.
        let by_binary = match (reuse, &digests) {
            (true, Some(d)) => runs.load_reference(&d.base, &base_sha),
            _ => None,
        }
        .map(|doc| narrowed(doc, common.shard));

        if let Some(d) = digests.as_ref().filter(|d| d.head == d.base) {
            println!(
                "gate: bench binaries identical (code and data match) — no measurable change possible"
            );
            // Loaded only when the cheap pass can use it.
            let load = || {
                let expected = reuse.then(|| headrun::expected_ids(common, &d.head_exe))??;
                let label = format!("binary {}", d.head);
                let doc = runs.load_covering(&d.head, &label, &expected)?;
                Some(Box::new((doc, label, expected)))
            };
            let early = if recording() { load() } else { None };
            let pass = identical_pass(
                reuse,
                early.is_some(),
                recording(),
                common.backend.as_deref(),
            );
            let exe = pass.plan.then_some(d.head_exe.as_path());
            let mut outcome = measure_head_with(exe, pass.args)
                .map(|(run, planned)| (run, pass.counted(planned)));
            let covering = match &outcome {
                Ok((_, false)) => early.or_else(load),
                _ => None,
            };
            if let Ok((run, false)) = &outcome
                && headrun::uncounted_complexity(run, covering.as_ref().map(|c| &c.0))
            {
                println!(
                    "gate: a complexity claim has no counted verdict stored for this binary; measuring with the gating counters"
                );
                outcome = measure_head_with(exe, &[]).map(|(run, _)| (run, true));
            }
            let counted = outcome.as_ref().is_ok_and(|(_, counted)| *counted);
            let run = outcome.map(|(run, _)| run);
            // Head shares that machine code, so a run of it is its
            // measurement too, and the next commit can reuse it from HEAD.
            let stored = covering.as_ref().map(|c| &c.0);
            if let Ok(pass) = &run
                && let Some(doc) = identical_pass_cache(stored, pass, counted, common.shard)
            {
                let settled = if counted {
                    settle("gate", "HEAD's run", pass, || measure(None, RECOUNT).ok())
                } else {
                    Settled::AsMeasured
                };
                match settled_doc(doc, &settled) {
                    Some(doc) if counted => store_head(&runs, doc, pass, &[head_dig.as_deref()]),
                    Some(doc) => store_identical(&runs, doc, pass, &[head_dig.as_deref()]),
                    None => {}
                }
            }
            let stored = covering.filter(|_| !counted);
            let identical = Ref::IdenticalBinaries {
                run,
                counted,
                stored,
            };
            return Ok((identical, digests, harness));
        }
        if let Some(doc) = by_binary {
            println!("gate: reusing a run measured from the same merge-base binary");
            return Ok((Ref::Cached(doc), digests, harness));
        }
        if !reuse {
            println!("gate: per-item reuse off (--no-reuse-base); measuring in full");
        }
        // A merge-base that counts another way would put its numbers in the
        // store under HEAD's method, so it neither reuses nor records.
        let same_method = harness.method().is_none();
        let item_reuse = digests
            .as_ref()
            .filter(|_| reuse && same_method)
            .and_then(|d| {
                let head = Bin::head(&d.head_exe, Some(&d.head));
                let base = Bin {
                    exe: &d.base_exe,
                    digest: Some(&d.base),
                    dir: Some(wt),
                    target_dir: wt_target.as_deref(),
                    env: base_env.as_ref().and_then(|e| e.as_ref().ok()),
                };
                Reuse::plan(
                    "gate",
                    common,
                    &runs,
                    &[head, base],
                    recording(),
                    "measuring in full",
                )
            });
        let measure_side = |side: usize, extra: &[&str]| -> Result<Run, String> {
            let dir = (side == 1).then_some(wt);
            let Some(reuse) = &item_reuse else {
                return measure(dir, extra);
            };
            let stamp = || buildstamp::capture(common.codegen_units_stamp().as_deref(), dir);
            reuse.measure_side(side, extra, stamp, |args| measure(dir, args))
        };
        let total = if walltime_advisory { 2 } else { 4 };
        let timed = |n: usize, side: usize, extra: &[&str]| {
            let start = Instant::now();
            let run = measure_side(side, extra);
            let label = ["head", "base"][side];
            println!(
                "gate: round {n}/{total} ({label}) done in {}",
                round_duration(start.elapsed())
            );
            run
        };
        let mut rounds = [timed(1, 0, &[]), timed(2, 1, &[])];
        let timing_rounds = (!walltime_advisory)
            .then(|| Box::new([timed(3, 0, TIMING_ONLY), timed(4, 1, TIMING_ONLY)]));
        if let Some(reuse) = &item_reuse {
            for (side, round) in rounds.iter_mut().enumerate() {
                if let Ok(run) = round {
                    reuse.finish(side, run);
                }
            }
            reused = reuse.reused_ids(1).cloned().collect();
        }
        // Read again here, where the merge-base worktree still exists; the
        // comparison itself uses the first reading either way.
        let (head_settled, base_settled) = match (&rounds[0], &rounds[1]) {
            (Ok(head), Ok(base)) => (
                settle("gate", "HEAD's run", head, || measure(None, RECOUNT).ok()),
                settle("gate", "the merge-base run", base, || {
                    measure(Some(wt), RECOUNT).ok()
                }),
            ),
            _ => (Settled::Refused, Settled::Refused),
        };
        Ok((
            Ref::Rounds(Box::new(rounds), timing_rounds, head_settled, base_settled),
            digests,
            harness,
        ))
    })?;
    let (outcome, digests, harness) = outcome;

    let sides = match outcome {
        Ref::NoBenchTarget(_) if !measure_new => return Ok(None),
        Ref::NoBenchTarget(at_head) => {
            println!("gate: no bench target at {refname}");
            let exe = head_exe.clone();
            let head = match (at_head, &head_exe) {
                (false, _) => NewTarget::Absent,
                (true, Some(exe))
                    if invoke::list_items(exe, None).is_some_and(|l| l.is_empty()) =>
                {
                    NewTarget::Empty
                }
                (true, _) => NewTarget::Registers,
            };
            let resolved = newly_measured(head, || {
                let cached = (reuse && !recording())
                    .then(|| head_from_runcache(&runs, head_dig.as_deref()))
                    .flatten();
                if let Some((head, from)) = cached {
                    println!("gate: reusing HEAD's own run, measured from {from}");
                    return Ok(head);
                }
                let head = measure_head(head_exe.as_deref().filter(|_| reuse))?;
                let settled = match runs.env() {
                    Some(_) => settle("gate", "HEAD's run", &head, || measure(None, RECOUNT).ok()),
                    None => Settled::AsMeasured,
                };
                cache_head(&runs, &head, head_dig.as_deref(), &settled);
                Ok(head)
            })?;
            return Ok(resolved.map(|r| Resolved { exe, ..r }));
        }
        // One cheap pass serves as both sides: real items and assertions,
        // zero deltas by construction.
        Ref::IdenticalBinaries {
            run,
            counted,
            stored,
        } => {
            let pass = run?;
            let (head, source) = identical_head(pass, counted, stored);
            return Ok(Some(Resolved {
                reference: ref_doc(&head),
                current: head,
                head: source,
                harness,
                exe: head_exe.clone(),
            }));
        }
        Ref::Cached(doc) => {
            let (head, settled, source) = head_from_cache_or(&runs, head_dig.as_deref(), || {
                let head = measure_head(head_exe.as_deref())?;
                let settled = settle("gate", "HEAD's run", &head, || measure(None, RECOUNT).ok());
                Ok((head, settled))
            })?;
            cache_head(&runs, &head, head_dig.as_deref(), &settled);
            return Ok(Some(Resolved {
                reference: doc,
                current: head,
                head: source,
                harness,
                exe: head_exe.clone(),
            }));
        }
        Ref::Rounds(rounds, timing_rounds, head_settled, base_settled) => {
            let [head1, base1] = *rounds;
            let (base, head) = match timing_rounds {
                Some(timing) => {
                    let [head2, base2] = *timing;
                    (combine_rounds(base1?, base2), combine_rounds(head1?, head2))
                }
                None => (base1?, head1?),
            };
            (base, head, head_settled, base_settled)
        }
    };
    let (base, head, head_settled, base_settled) = sides;
    let mut reference = ref_doc(&base);
    for id in &reused {
        reference["items"][id.as_str()]["reused_from"] = serde_json::json!("item cache");
    }
    mark_harness(&mut reference, &harness);
    if let Some(mut stored) =
        settled_doc(reference.clone(), &base_settled).filter(|_| harness.method().is_none())
    {
        headrun::stamp(&mut stored);
        if base_known {
            runs.store(&base_sha, &stored);
        }
        // Keyed by machine code too, so a later merge-base building the same
        // binary hits even if never gated. Stored regardless of `reuse`.
        if let Some(d) = &digests {
            runs.store(&d.base, &stored);
        }
    }
    if let Some(doc) = settled_doc(ref_doc(&head), &head_settled) {
        let text = digests.as_ref().map(|d| d.head.as_str());
        store_head(&runs, doc, &head, &[head_dig.as_deref(), text]);
    }
    Ok(Some(Resolved {
        reference,
        current: head,
        head: HeadRun::Measured,
        harness,
        exe: head_exe,
    }))
}

/// On failure, produce a callgrind function-level report per regressed item
/// (best effort). No `valgrind --version` pre-check: it reports success on
/// AVX-512 hosts where callgrind SIGILLs — only running the real binary
/// (which the runner's own probe does) tells the truth. One failed attempt
/// disables triage for the rest of the run instead of failing per item.
fn triage(common: &CommonArgs, failing_ids: &[String]) {
    let Ok(root) = invoke::workspace_root() else {
        return;
    };
    let dir = root.join(".soothfast").join("triage");
    let _ = std::fs::create_dir_all(&dir);
    for id in failing_ids.iter().take(3) {
        match invoke::run_bench_raw(common, &["--triage", id]) {
            Ok(report) => {
                let path = dir.join(format!("{}.txt", id.replace("::", "_")));
                if std::fs::write(&path, report).is_ok() {
                    println!("triage: {}", path.display());
                }
            }
            Err(e) => {
                println!("triage: skipped (callgrind unavailable here: {e})");
                return;
            }
        }
    }
}

fn err(msg: &str) -> i32 {
    eprintln!("soothfast: {msg}");
    2
}

/// `err` for a gate that never reached a verdict. Its `--save-baseline`
/// still names the package's items as gate-failed, so no claim on them reads
/// as measured.
fn errored(g: &GateArgs, exe: Option<&std::path::Path>, msg: &str) -> i32 {
    if let (Some(name), Ok(root)) = (&g.save_baseline, invoke::workspace_root())
        && g.common.backend.as_deref() != Some("buildcost")
    {
        let built = exe
            .map(std::path::Path::to_path_buf)
            .or_else(|| crate::benchexe::stored(&g.common, None, None, invoke::Build::Measure));
        let listed = crate::benchexe::listing(&g.common, built.as_deref());
        if let Err(e) = &listed {
            println!("gate: {e}, so its gate-failed entry names only the items it owned before");
        }
        errored_at(&root, name, &g.common, listed.ok().as_deref(), msg);
    }
    err(msg)
}

/// `errored` for a gate whose config did not apply: a build without that
/// config is not HEAD's binary, so nothing is listed and every item is.
fn errored_unbuilt(g: &GateArgs, msg: &str) -> i32 {
    if let (Some(name), Ok(root)) = (&g.save_baseline, invoke::workspace_root())
        && g.common.backend.as_deref() != Some("buildcost")
    {
        errored_at(&root, name, &g.common, None, msg);
    }
    err(msg)
}

/// The gate-failed stub `errored` writes into baseline `name` under `root`.
fn errored_at(
    root: &std::path::Path,
    name: &str,
    common: &CommonArgs,
    listed: Option<&[String]>,
    msg: &str,
) {
    if let Err(e) = save_errored(root, name, common, listed, msg) {
        eprintln!("soothfast: failed to save baseline: {e}");
    }
}

/// The ratchet baseline `name` under `root`.
fn ratchet_in(root: &std::path::Path, name: &str) -> Result<Value, String> {
    match invoke::load_baseline_in(root, name) {
        Ok(Some(r)) => Ok(r),
        Ok(None) => Err(format!("no ratchet baseline named {name:?}")),
        Err(e) => Err(format!("failed to load ratchet baseline: {e}")),
    }
}

/// Like `err`, but for a `gate accept --only` invocation that recorded
/// nothing: distinct from `2` (a usage/config error) so a CI step doesn't
/// mistake "named benches, none eligible" for full success or bad usage.
fn partial(msg: &str) -> i32 {
    eprintln!("soothfast: {msg}");
    3
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{errored_at, ratchet_in, run_imported};

    use crate::gate::args::{GateArgs, ImportArgs};
    use crate::gate::test_support::scratch;

    use crate::invoke::{self, CommonArgs, ItemMetrics, Run};

    #[test]
    fn a_gate_whose_bench_did_not_build_covers_its_package_for_unchanged_claims_only() {
        let root = scratch("bench-unbuilt");
        let stale = json!({
            "version": 1,
            "items": { "pkg::old": { "alloc": { "allocs": 1, "bytes": 8 } } },
            "owners": { "pkg": ["pkg::old"] },
        });
        invoke::write_baseline_in(&root, "head", &stale).unwrap();
        let common = CommonArgs {
            pkg: Some("pkg".into()),
            ..CommonArgs::default()
        };
        errored_at(&root, "head", &common, None, "cargo bench failed");
        let saved = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
        assert_eq!(saved["items"], json!({}));
        let expr = "pkg::old.alloc.allocs <= 1";
        let doc =
            soothfast_docs::markdown::scan(&format!("<!-- soothfast:claim {expr} -->\n")).unwrap();
        let docs = [("a.md", &doc)];
        let unchanged = || {
            Ok(std::collections::BTreeSet::from([(
                "a.md".to_string(),
                expr.to_string(),
            )]))
        };
        assert_eq!(
            crate::docs_claims::check(&docs, Some(&saved), unchanged),
            Ok(0)
        );
        let edited = || Ok(std::collections::BTreeSet::new());
        assert_eq!(
            crate::docs_claims::check(&docs, Some(&saved), edited),
            Ok(1)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_ratchet_error_replaces_the_package_s_saved_numbers_with_a_failed_entry() {
        let root = scratch("ratchet-error");
        let stale = json!({
            "version": 1,
            "items": { "pkg::old": { "alloc": { "allocs": 1, "bytes": 8 } } },
            "owners": { "pkg": ["pkg::old"] },
        });
        invoke::write_baseline_in(&root, "head", &stale).unwrap();
        let e = ratchet_in(&root, "release").unwrap_err();
        let common = CommonArgs {
            pkg: Some("pkg".into()),
            ..CommonArgs::default()
        };
        let listed = ["pkg::new".to_string()];
        errored_at(&root, "head", &common, Some(&listed), &e);
        let saved = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
        assert_eq!(saved["items"], json!({}));
        assert_eq!(saved["unmeasured"][0]["reason"], "gate-failed");
        assert_eq!(saved["unmeasured"][0]["items"], json!(["pkg::new"]));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn pyperf(seconds: f64) -> String {
        pyperf_spread(seconds, 0.0)
    }

    fn pyperf_spread(seconds: f64, spread: f64) -> String {
        let (lo, hi) = (seconds * (1.0 - spread), seconds * (1.0 + spread));
        let runs = json!([{ "values": [lo, seconds, hi] }]);
        json!({ "benchmarks": [{ "metadata": { "name": "b" }, "runs": runs }] }).to_string()
    }

    fn imported_gate(name: &str, current: f64, config: &str, baseline: bool) -> i32 {
        imported_gate_with(name, current, config, baseline, |_| {})
    }

    fn imported_gate_with(
        name: &str,
        current: f64,
        config: &str,
        baseline: bool,
        tweak: impl FnOnce(&mut GateArgs),
    ) -> i32 {
        let root = std::env::temp_dir().join(format!("soothfast-gate-import-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("soothfast.toml"), config).unwrap();
        let file = |stem: &str, text: String| {
            let path = root.join(stem);
            std::fs::write(&path, text).unwrap();
            ("pyperf".to_string(), path.display().to_string())
        };
        let reference = file("ref.json", pyperf(0.001));
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
            import: ImportArgs {
                current: Some(file("cur.json", pyperf(current))),
                reference: Some(reference.clone()),
                prefix: None,
                walltime: None,
                baseline_given: false,
            },
        };
        tweak(&mut g);
        if baseline {
            let (run, prefix) = crate::import::read(&reference.0, &reference.1, None).unwrap();
            let scope = invoke::SaveScope::Imported(prefix);
            invoke::save_baseline_in(&root, "base", &run, scope).unwrap();
            let other = ItemMetrics {
                median_ns: Some(1.0),
                imported: Some("jmh".into()),
                ..ItemMetrics::default()
            };
            let mut other_run = Run::default();
            other_run.items.insert("jmh::elsewhere".into(), other);
            let scope = invoke::SaveScope::Imported("jmh".into());
            invoke::save_baseline_in(&root, "base", &other_run, scope).unwrap();
            g.import.reference = None;
        }
        let code = run_imported(&g, &root);
        let _ = std::fs::remove_dir_all(&root);
        code
    }

    #[test]
    fn an_import_filter_matching_nothing_is_refused() {
        let code = imported_gate_with("filter-none", 0.0013, "", false, |g| {
            g.common.filter = Some("nothing".into());
        });
        assert_eq!(code, 2);
    }

    #[test]
    fn an_import_filter_gates_the_matching_items() {
        let code = imported_gate_with("filter-some", 0.0013, "", false, |g| {
            g.common.filter = Some("b".into());
        });
        assert_eq!(code, 1);
    }

    #[test]
    fn an_import_rejects_shard_and_package() {
        let shard = imported_gate_with("shard", 0.001, "", false, |g| {
            g.common.shard = Some(invoke::Shard::parse("1/2").unwrap());
        });
        let pkg = imported_gate_with("pkg", 0.001, "", false, |g| {
            g.common.pkg = Some("mylib".into());
        });
        assert_eq!((shard, pkg), (2, 2));
    }

    #[test]
    fn an_import_rejects_a_baseline_beside_a_reference_import() {
        let code = imported_gate_with("both", 0.001, "", false, |g| {
            g.baseline = "base".into();
            g.import.baseline_given = true;
        });
        assert_eq!(code, 2);
    }

    #[test]
    fn an_imported_run_30_percent_slower_fails() {
        assert_eq!(imported_gate("slow", 0.0013, "", false), 1);
    }

    fn spread_gate(name: &str, spread: f64, current: f64) -> i32 {
        imported_gate_with(name, current, "", false, |g| {
            let paths = [&g.import.current, &g.import.reference];
            for (path, seconds) in paths.into_iter().zip([current, 0.001]) {
                let path = &path.as_ref().unwrap().1;
                std::fs::write(path, pyperf_spread(seconds, spread)).unwrap();
            }
        })
    }

    #[test]
    fn imports_with_4_percent_dispersion_pass_at_9_percent() {
        assert_eq!(spread_gate("noisy-9", 0.04, 0.00109), 0);
    }

    #[test]
    fn dispersion_widens_the_walltime_limit_past_the_fixed_10_percent() {
        assert_eq!(spread_gate("noisy-11", 0.04, 0.00111), 0);
        assert_eq!(spread_gate("quiet-11", 0.0001, 0.00111), 1);
    }

    #[test]
    fn imports_with_4_percent_dispersion_still_fail_at_30_percent() {
        assert_eq!(spread_gate("noisy-30", 0.04, 0.0013), 1);
    }

    #[test]
    fn imports_with_near_zero_dispersion_still_fail_at_30_percent() {
        assert_eq!(spread_gate("quiet-30", 0.0001, 0.0013), 1);
    }

    #[test]
    fn an_imported_run_2_percent_slower_passes() {
        assert_eq!(imported_gate("flat", 0.00102, "", false), 0);
    }

    #[test]
    fn an_imported_run_gates_against_a_stored_baseline_of_its_own_prefix() {
        assert_eq!(imported_gate("base-slow", 0.0013, "", true), 1);
        assert_eq!(imported_gate("base-flat", 0.00102, "", true), 0);
    }

    #[test]
    fn advisory_walltime_lets_an_imported_regression_through() {
        let advisory = "[gate]\nwalltime = \"advisory\"\n";
        assert_eq!(imported_gate("advisory", 0.0013, advisory, false), 0);
    }
}
