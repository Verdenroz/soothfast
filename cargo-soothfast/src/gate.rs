//! The regression gate: re-measure, compare against a stored baseline (or a
//! merge-base worktree with `--against-ref`) with per-metric thresholds,
//! exit non-zero on regression. On failure, emits a callgrind triage
//! artifact when valgrind is available.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::buildstamp;
use crate::gate_config;
use crate::gate_lock;
use crate::gate_save::{HeadRun, save_errored, save_head};
use crate::headrun::{
    self, RECOUNT, Settled, binary_digest, cache_head, cache_head_doc, head_from_runcache, ref_doc,
    settle, settled_doc,
};
use crate::import;
use crate::invoke::{self, CommonArgs, ItemMetrics, Run};
use crate::itemreuse::{Bin, Reuse};
use crate::workspace;

/// Deterministic counters gate tight.
pub const INSTRUCTIONS_THRESHOLD_PCT: f64 = 5.0;
const IR_THRESHOLD_PCT: f64 = 5.0;
/// Walltime medians gate at +10%, or higher on noisy runners (see
/// `walltime_limit`).
const WALLTIME_THRESHOLD_PCT: f64 = 10.0;
/// Multiple of the A/A noise floor the walltime threshold must stay above.
/// 3x keeps the threshold at ≥3 sigma, where a suite of a few hundred items
/// expects well under one false positive per run.
const WALLTIME_NOISE_MARGIN: f64 = 3.0;
/// Allocation counts/bytes and binary size are deterministic; gate at +5%.
pub const ALLOC_THRESHOLD_PCT: u64 = 5;
const SIZE_THRESHOLD_PCT: u64 = 5;
/// Poll/wake counts on the counting executor are exact; gate them like
/// allocations. This is the regression class instruction counts miss — a
/// future that polls twice as often can have an identical `Ir`.
const ASYNC_THRESHOLD_PCT: u64 = 5;
/// Compile time is noisy: soft (warn-only) at +25%.
const BUILD_MS_SOFT_PCT: f64 = 25.0;
/// A deterministic counter within this of the reference reads as "flat" —
/// wide enough for perfcnt's tiny run-to-run wobble, far below any real change.
pub(crate) const COUNTER_FLAT_PCT: f64 = 0.5;
/// Absolute floor alongside `COUNTER_FLAT_PCT`: below ~30K instructions the
/// 0.5% cutoff is tighter than perfcnt's own run-to-run wobble.
pub(crate) const COUNTER_FLAT_ABS: u64 = 150;
/// `gate accept --headroom` floor for deterministic counters: a delta must
/// clear this before it counts as "close to failing," so a bench parked
/// near the line by measurement noise alone never qualifies.
const HEADROOM_COUNTER_FLOOR_PCT: f64 = 3.0 * COUNTER_FLAT_PCT;

/// Effective walltime threshold: the fixed +10%, raised to 3x the measured
/// A/A noise floor when that is higher. A fixed threshold is ~2 sigma on a
/// noisy runner — several false positives per run across a few hundred
/// items — while dropping walltime entirely would leave such runners
/// unmonitored. Scaling keeps every run gated at ≥3 sigma.
fn walltime_limit(noise_pct: f64) -> f64 {
    WALLTIME_THRESHOLD_PCT.max(noise_pct * WALLTIME_NOISE_MARGIN)
}

/// Relative walltime dispersion of an imported item, the larger of its two
/// sides, in percent. A harness result carries no A/A run, so its own spread
/// stands in for the noise floor. Zero for a natively measured item.
fn imported_noise_pct(old: &Value, cur: &ItemMetrics) -> f64 {
    if cur.imported.is_none() {
        return 0.0;
    }
    let spread = |mad: Option<f64>, median: Option<f64>| match (mad, median) {
        (Some(mad), Some(median)) if median > 0.0 => mad / median * 100.0,
        _ => 0.0,
    };
    spread(cur.mad_ns, cur.median_ns).max(spread(
        old["walltime"]["mad_ns"].as_f64(),
        old["walltime"]["median_ns"].as_f64(),
    ))
}

/// A bench whose loop body is only a few instructions wide cannot survive a
/// tight relative threshold across a refactor: one spilled register is
/// already several percent. `tolerance` widens its own gate.
fn counter_limit(default_pct: f64, cur: &ItemMetrics) -> f64 {
    default_pct.max(cur.tolerance_pct.unwrap_or(0.0))
}

struct GateArgs {
    common: CommonArgs,
    baseline: String,
    ratchet: Option<String>,
    against_ref: Option<String>,
    deps: bool,
    allow_gone: bool,
    allow_harness_change: bool,
    matrix: String,
    save_baseline: Option<String>,
    reuse_base: bool,
    import: ImportArgs,
}

/// `--import`: gate result files from another harness instead of measuring.
/// Each side is a `(format, file)` pair.
#[derive(Default)]
struct ImportArgs {
    current: Option<(String, String)>,
    reference: Option<(String, String)>,
    prefix: Option<String>,
    walltime: Option<String>,
    baseline_given: bool,
}

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
    let walltime_advisory = match gate_config::apply(&mut g.common) {
        Ok(cfg) => cfg.walltime_advisory,
        Err(e) => return errored_unbuilt(&g, &e),
    };
    if let Some(shard) = g.common.shard {
        println!("gate: shard {shard}: gating the items it owns");
    }

    let mut regressions = 0u32;
    let mut failing_ids: Vec<String> = Vec::new();

    let resolved = match resolve(&g, walltime_advisory) {
        Ok(Some(resolved)) => resolved,
        // Nothing at the ref to regress against, so HEAD's own assertions
        // decide, and a passing run is saved like any other.
        Ok(None) => {
            println!(
                "gate: no bench target at {} for {} — newly measured, checking its own claims",
                g.against_ref.as_deref().unwrap_or("?"),
                g.common.pkg.as_deref().unwrap_or("this package")
            );
            match newly_measured(&g.common) {
                Ok(resolved) => resolved,
                Err(e) => return errored(&g, None, &e),
            }
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
    match g.common.shard {
        Some(shard) if current.items.is_empty() && g.common.filter.is_none() => {
            println!("gate: shard {shard} owns no item now; checking none it owned is gone");
        }
        _ if current.items.is_empty() => {
            return errored(
                &g,
                exe.as_deref(),
                "measured 0 items — check --filter / registry setup; refusing to gate",
            );
        }
        _ => {}
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
        let saved = invoke::workspace_root()
            .map_err(|e| e.to_string())
            .and_then(|root| {
                let listed = exe
                    .as_deref()
                    .and_then(|exe| invoke::listed_ids(exe, g.common.filter.as_deref()));
                let listed = listed.as_deref();
                save_head(&root, name, &current, &head, &g.common, listed, failures)
            });
        if let Err(e) = saved {
            eprintln!("soothfast: failed to save baseline: {e}");
            return 1;
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
    let walltime_advisory = match (&g.import.walltime, gate_config::load(root)) {
        (Some(mode), _) => mode == "advisory",
        (None, Ok(cfg)) => cfg.walltime_advisory,
        (None, Err(e)) => return err(&e),
    };
    let prefix = g.import.prefix.as_deref();
    let (mut current, prefix) = match import::read(format, file, prefix) {
        Ok(read) => read,
        Err(e) => return err(&e),
    };
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

/// How to recover when a regression was measured against a stored perfcnt
/// reference, which one bad reading could have skewed. Callgrind is exact,
/// so a reused callgrind reference needs no such doubt.
fn reuse_hint(reference: &Value, backend: Option<&str>, regressions: u32) -> Option<String> {
    let from = reference["reused_from"].as_str()?;
    (regressions > 0 && backend == Some("perfcnt")).then(|| {
        format!("gate: reference reused from {from}; rerun with --no-reuse-base to re-measure")
    })
}

/// Put the mismatch in front of the deltas it may have produced: a
/// reference built against another harness measures another protocol, so
/// its numbers are not this change's in either direction.
fn harness_note(harness: &invoke::HarnessSync) {
    if !harness.unpinned().is_empty() {
        println!(
            "gate: HARNESS MISMATCH — the merge-base could not be pinned to HEAD's soothfast ({}); \
             deltas below may be the harness, not this change",
            harness.unpinned().join(", ")
        );
    }
    if let Some(method) = harness.method_note() {
        println!("gate: HARNESS MISMATCH — {method}; comparison is not conclusive");
    }
}

/// The reasons a harness mismatch has, for one line.
fn harness_reasons(harness: &invoke::HarnessSync) -> String {
    let mut reasons = harness.unpinned().to_vec();
    reasons.extend(harness.method_note());
    reasons.join(", ")
}

/// What the verdict line adds about the mismatch. The PR comment carries
/// only the tail of the output, so the header alone would not reach it.
fn harness_verdict(harness: &invoke::HarnessSync, regressed: bool) -> String {
    if !harness.is_mismatched() {
        return String::new();
    }
    let reasons = harness_reasons(harness);
    if regressed {
        format!(
            " — measured against a different harness ({reasons}); \
             pass --allow-harness-change if the harness bump explains it"
        )
    } else {
        format!(
            " — but measured against a different harness ({reasons}), \
             so the comparison is not conclusive"
        )
    }
}

/// Whether a regression measured against an unpinned harness is waived.
/// Only comparison failures qualify: an assertion holds HEAD against its
/// own declared numbers, which the reference harness has no say over.
fn harness_waiver(harness: &invoke::HarnessSync, allow: bool, regressions: u32) -> bool {
    regressions > 0 && harness.is_mismatched() && allow
}

/// Leave the mismatch beside the triage reports in `dir`, so a downloaded
/// artifact carries the reason its numbers may not be the code's. Removed
/// when the pin held: a stale file would mislabel a valid comparison.
fn triage_harness_note(dir: &std::path::Path, harness: &invoke::HarnessSync) {
    let path = dir.join("harness-mismatch.txt");
    if !harness.is_mismatched() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let _ = std::fs::create_dir_all(dir);
    let reasons: String = harness
        .unpinned()
        .iter()
        .cloned()
        .chain(harness.method_note())
        .map(|r| format!("  {r}\n"))
        .collect();
    let _ = std::fs::write(
        &path,
        format!(
            "harness mismatch\n\n\
             The reference bench does not measure the way HEAD's does (a soothfast version the\n\
             merge-base could not be pinned to, or the callgrind method):\n\n\
             {reasons}\n\
             Deltas in this run may be that harness change rather than the measured code.\n"
        ),
    );
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
    if gate_config::apply(&mut common).is_ok() && common.features.is_none() {
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

fn against_ref_in(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--against-ref" {
            return it.next().cloned();
        }
    }
    None
}

/// Packages named by repeated `-p`, in the order given.
fn packages_in(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if matches!(a.as_str(), "-p" | "--package")
            && let Some(p) = it.next()
        {
            out.push(p.clone());
        }
    }
    out
}

/// `args` with every `-p` collapsed to the single named package.
fn with_package(args: &[String], pkg: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if matches!(a.as_str(), "-p" | "--package") {
            it.next();
            continue;
        }
        out.push(a.clone());
    }
    out.push("-p".into());
    out.push(pkg.into());
    out
}

/// The two sides of one comparison, plus what the reader needs to know
/// about how much the comparison is worth.
struct Resolved {
    reference: Value,
    current: Run,
    head: HeadRun,
    harness: invoke::HarnessSync,
    /// HEAD's probed bench executable, which lists the items a save names.
    exe: Option<std::path::PathBuf>,
}

/// HEAD's run from the run cache, or measured now with `measure`.
fn head_from_cache_or(
    runs: &crate::runcache::Runs,
    digest: Option<&str>,
    measure: impl FnOnce() -> Result<(Run, Settled), String>,
) -> Result<(Run, Settled, HeadRun), String> {
    if let Some((head, from)) = head_from_runcache(runs, digest) {
        println!("gate: reusing HEAD's own run, measured from {from}");
        return Ok((head, Settled::AsMeasured, HeadRun::Stored(from)));
    }
    let (head, settled) = measure()?;
    Ok((head, settled, HeadRun::Measured))
}

/// Measure or load both sides of a gate run as a [`Resolved`]. `Ok(None)`
/// only in `--against-ref` mode: a crate newly measured has nothing at the
/// ref to compare against.
fn resolve(g: &GateArgs, walltime_advisory: bool) -> Result<Option<Resolved>, String> {
    if g.common.backend.as_deref() == Some("buildcost") {
        let Some(pkg) = g.common.pkg.clone() else {
            return Err("--backend buildcost requires -p PKG".into());
        };
        let head_target = invoke::buildcost_target_dir().map_err(|e| e.to_string())?;
        let current = crate::buildcost::measure(&pkg, &g.matrix, Some(&head_target))?;
        let reference = if let Some(refname) = &g.against_ref {
            let base_target = invoke::buildcost_base_target_dir().map_err(|e| e.to_string())?;
            let base = invoke::with_merge_base_worktree(refname, |wt| {
                invoke::sync_untracked_cargo_config(wt)?;
                crate::buildcost::measure_in(&pkg, &g.matrix, Some(wt), Some(&base_target))
            })?;
            let mut doc = serde_json::json!({ "version": 1 });
            doc["items"] = invoke::run_to_items_value(&base);
            doc
        } else {
            let mut baseline = invoke::load_baseline(&g.baseline)
                .map_err(|e| format!("failed to load baseline: {e}"))?
                .ok_or_else(|| format!("no baseline named {:?}", g.baseline))?;
            // Only buildcost items are in play; drop bench entries from the view.
            if let Some(map) = baseline["items"].as_object_mut() {
                map.retain(|k, _| k.starts_with("buildcost::"));
            }
            baseline
        };
        return Ok(Some(Resolved {
            reference,
            current,
            head: HeadRun::Measured,
            harness: invoke::HarnessSync::default(),
            exe: None,
        }));
    }
    if let Some(refname) = &g.against_ref {
        return measure_ref_interleaved(&g.common, refname, g.reuse_base, walltime_advisory);
    }
    let records = invoke::run_bench(&g.common, &[]).map_err(|e| e.to_string())?;
    let mut current = invoke::collect(&records);
    current.build = Some(buildstamp::capture(
        g.common.codegen_units_stamp().as_deref(),
        None,
    ));
    let baseline = invoke::load_baseline(&g.baseline)
        .map_err(|e| format!("failed to load baseline: {e}"))?
        .ok_or_else(|| {
            format!(
                "no baseline named {:?} — create one with `cargo soothfast measure --save-baseline {}`",
                g.baseline, g.baseline
            )
        })?;
    Ok(Some(Resolved {
        reference: baseline,
        current,
        head: HeadRun::Measured,
        harness: invoke::HarnessSync::default(),
        exe: invoke::bench_executable(&g.common, None, None),
    }))
}

struct AcceptArgs {
    common: CommonArgs,
    baseline: String,
    against_ref: Option<String>,
    matrix: String,
    justification: Option<String>,
    only: Vec<String>,
    headroom: Option<f64>,
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
    let mut a = AcceptArgs {
        common: CommonArgs::default(),
        baseline: "base".into(),
        against_ref: None,
        matrix: "default".into(),
        justification: None,
        only: Vec::new(),
        headroom: None,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if a.common.try_parse(arg, &mut it) {
            continue;
        }
        match arg.as_str() {
            "--baseline" => match it.next() {
                Some(n) => a.baseline = n.clone(),
                None => return err("--baseline needs a name"),
            },
            "--against-ref" => match it.next() {
                Some(r) => a.against_ref = Some(r.clone()),
                None => return err("--against-ref needs a git ref"),
            },
            "--features-matrix" => match it.next() {
                Some(m) => a.matrix = m.clone(),
                None => return err("--features-matrix needs combos like \"default;full\""),
            },
            "--justification" => match it.next() {
                Some(j) => a.justification = Some(j.clone()),
                None => return err("--justification needs text"),
            },
            "--only" => match it.next() {
                Some(id) => a.only.push(id.clone()),
                None => return err("--only needs a bench id"),
            },
            "--headroom" => match it.next().map(|v| v.parse::<f64>()) {
                Some(Ok(pct)) => a.headroom = Some(pct),
                _ => return err("--headroom needs a percentage number"),
            },
            other => return err(&format!("unknown gate accept arg {other:?}")),
        }
    }
    let walltime_advisory = match gate_config::apply(&mut a.common) {
        Ok(cfg) => cfg.walltime_advisory,
        Err(e) => return err(&e),
    };
    let Some(justification) = a.justification.clone() else {
        return err("gate accept needs --justification \"...\"");
    };

    let g = GateArgs {
        common: a.common,
        baseline: a.baseline,
        ratchet: None,
        against_ref: a.against_ref,
        deps: false,
        allow_gone: false,
        allow_harness_change: false,
        matrix: a.matrix,
        save_baseline: None,
        reuse_base: true,
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
    if current.items.is_empty() {
        return err("measured 0 items — check --filter / registry setup; refusing to accept");
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

/// Print HEAD's checked claims and count the ones that failed.
fn assertion_failures(current: &Run, failing_ids: &mut Vec<String>) -> u32 {
    let mut failures = 0u32;
    for a in &current.assertions {
        let verdict = if a.ok { "ok" } else { "FAIL" };
        if !a.ok {
            failures += 1;
            failing_ids.push(a.id.clone());
        }
        println!("{verdict:<5} {} assert {}: {}", a.id, a.kind, a.detail);
    }
    failures
}

/// Record the verdict for `report render` (badge) — best effort, never fatal.
fn write_gate_status(failures: u32) {
    let Ok(root) = invoke::workspace_root() else {
        return;
    };
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let doc = serde_json::json!({
        "passed": failures == 0,
        "failures": failures,
        "unix": unix,
    });
    let dir = root.join(".soothfast");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(
        dir.join("gate-status.json"),
        serde_json::to_string_pretty(&doc).unwrap_or_default(),
    );
}

/// HEAD of a crate with no bench target at the merge-base. Cached only when
/// its assertions hold: a cached run keeps none for a later gate to check.
fn newly_measured(common: &CommonArgs) -> Result<Resolved, String> {
    let stamp = || buildstamp::capture(common.codegen_units_stamp().as_deref(), None);
    let head_stamp = stamp();
    let (digest, runs, exe) = headrun::probe("gate", common, &head_stamp);
    let measure = |extra: &[&str]| -> Result<Run, String> {
        let records = invoke::run_bench(common, extra).map_err(|e| e.to_string())?;
        let mut run = invoke::collect(&records);
        run.build = Some(stamp());
        Ok(run)
    };
    let head_bin = exe.as_deref().map(|exe| Bin::head(exe, digest.as_deref()));
    let (head, _) = Reuse::measure_head(
        "gate",
        common,
        &runs,
        head_bin,
        recording(),
        &[],
        stamp,
        measure,
    )?;
    if head.assertions.iter().all(|a| a.ok) {
        let settled = settle("gate", "HEAD's run", &head, || measure(RECOUNT).ok());
        cache_head(&runs, &head, digest.as_deref(), &settled);
    }
    Ok(Resolved {
        exe,
        ..self_compared(head)
    })
}

/// `head` as both sides of a comparison, so every delta is zero.
fn self_compared(head: Run) -> Resolved {
    Resolved {
        reference: ref_doc(&head),
        current: head,
        head: HeadRun::Measured,
        harness: invoke::HarnessSync::default(),
        exe: None,
    }
}

/// Measure the merge-base with HEAD in a temp worktree, interleaving rounds
/// (head, base, head, base) tango-style so slow environmental drift cancels;
/// per-metric minima across each side's rounds form the comparison values.
/// Interleaving is a timing concern: the deterministic gating counters
/// (perfcnt/callgrind) don't drift, so the second round of each side skips
/// them and re-measures only the timing-sensitive backends. Advisory walltime
/// can't fail on that second sample, so it skips the second round.
/// `Ok(None)` when the merge-base has no such bench target: the crate is
/// newly measured, so there is nothing on that side to compare against.
/// A merge-base already measured under the same conditions is served from
/// the run cache instead of being rebuilt, unless `reuse` is false.
fn measure_ref_interleaved(
    common: &CommonArgs,
    refname: &str,
    reuse: bool,
    walltime_advisory: bool,
) -> Result<Option<Resolved>, String> {
    const TIMING_ONLY: &[&str] = &["--skip-gating-counters"];
    enum Ref {
        NoBenchTarget,
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
            return Ok((Ref::NoBenchTarget, None, invoke::HarnessSync::default()));
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
        };

        if let Some(d) = digests.as_ref().filter(|d| d.head == d.base) {
            println!(
                "gate: bench binaries identical (code and data match) — no measurable change possible"
            );
            let pass = identical_pass(
                reuse,
                by_binary.is_some(),
                recording(),
                common.backend.as_deref(),
            );
            let exe = pass.plan.then_some(d.head_exe.as_path());
            let outcome = measure_head_with(exe, pass.args);
            let counted = outcome
                .as_ref()
                .is_ok_and(|(_, planned)| pass.counted(*planned));
            let run = outcome.map(|(run, _)| run);
            // Head shares that machine code, so a run of it is its
            // measurement too, and the next commit can reuse it from HEAD.
            if let Ok(pass) = &run
                && let Some(doc) = identical_pass_cache(by_binary.as_ref(), pass, counted)
            {
                let settled = if counted {
                    settle("gate", "HEAD's run", pass, || measure(None, RECOUNT).ok())
                } else {
                    Settled::AsMeasured
                };
                if let Some(doc) = settled_doc(doc, &settled) {
                    cache_head_doc(&runs, &doc, head_dig.as_deref());
                }
            }
            let label = format!("binary {}", d.head);
            let stored = (!counted)
                .then(|| runs.load_covering(&d.head, &label))
                .flatten()
                .zip(headrun::expected_ids(common, &d.head_exe))
                .map(|(doc, expected)| Box::new((doc, label, expected)));
            return Ok((
                Ref::IdenticalBinaries {
                    run,
                    counted,
                    stored,
                },
                digests,
                harness,
            ));
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
        Ref::NoBenchTarget => return Ok(None),
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
    if let Some(stored) =
        settled_doc(reference.clone(), &base_settled).filter(|_| harness.method().is_none())
    {
        if base_known {
            runs.store(&base_sha, &stored);
        }
        // Keyed by machine code too, so a later merge-base building the same
        // binary hits even if never gated. Stored regardless of `reuse`.
        if let Some(d) = &digests {
            runs.store(&d.base, &stored);
        }
    }
    if let (Some(d), Some(stored)) = (&digests, settled_doc(ref_doc(&head), &head_settled)) {
        runs.store(&d.head, &stored);
    }
    cache_head(&runs, &head, head_dig.as_deref(), &head_settled);
    Ok(Some(Resolved {
        reference,
        current: head,
        head: HeadRun::Measured,
        harness,
        exe: head_exe,
    }))
}

/// Where a stored reference remembers that its harness could not be pinned,
/// so a run served from the cache carries the mark of the run that measured
/// it rather than reading as a clean comparison.
const HARNESS_KEY: &str = "harness_unpinned";

/// Where it remembers the callgrind methods of the two sides, `[base, head]`.
const HARNESS_METHOD_KEY: &str = "harness_method";

fn mark_harness(doc: &mut Value, harness: &invoke::HarnessSync) {
    if !harness.unpinned().is_empty() {
        doc[HARNESS_KEY] = serde_json::json!(harness.unpinned());
    }
    if let Some(method) = harness.method() {
        doc[HARNESS_METHOD_KEY] = serde_json::json!(method);
    }
}

fn harness_of(doc: &Value) -> invoke::HarnessSync {
    let labels = doc[HARNESS_KEY]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let sync = invoke::HarnessSync::from_unpinned(labels);
    match (
        doc[HARNESS_METHOD_KEY][0].as_str(),
        doc[HARNESS_METHOD_KEY][1].as_str(),
    ) {
        (Some(base), Some(head)) => sync.with_method(base, head),
        _ => sync,
    }
}

/// Whether this gate runs to record HEAD as a reference (`SOOTHFAST_RECORD=1`,
/// set by the action on default-branch pushes) rather than to judge a change.
/// An environment variable, so older CLIs the action installs ignore it.
pub fn recording() -> bool {
    std::env::var("SOOTHFAST_RECORD").is_ok_and(|v| v == "1")
}

/// Runner args for the single pass taken when both sides' binaries match.
/// A pass that `record`s a binary nothing is stored for keeps the gating
/// counters, so it can be stored as that binary's run. A backend named on the
/// command line may be one of the gating counters, which measures nothing once
/// those counters are skipped, so it keeps them.
fn identical_pass_args(backend: Option<&str>, record: bool) -> &'static [&'static str] {
    match backend {
        _ if record => &[],
        Some("perfcnt" | "callgrind") => &[],
        _ => &["--skip-gating-counters"],
    }
}

/// How the identical-binaries branch takes its one pass: planned per item
/// when `plan` (falling back to `args` when no plan applies), else with `args`.
#[derive(Debug, PartialEq)]
struct IdenticalPass {
    plan: bool,
    args: &'static [&'static str],
}

impl IdenticalPass {
    /// Whether the pass carries the gating counters, `planned` saying the plan
    /// was used. Only then is its run a baseline and a stored measurement.
    fn counted(&self, planned: bool) -> bool {
        planned || self.args.is_empty()
    }
}

/// A run stored for the binary is carried forward by a plan too, which splices
/// its items and their claims from the item store instead of measuring them.
fn identical_pass(
    reuse: bool,
    stored: bool,
    recording: bool,
    backend: Option<&str>,
) -> IdenticalPass {
    IdenticalPass {
        plan: reuse,
        args: identical_pass_args(backend, !stored && recording),
    }
}

/// What the identical-binaries branch keeps as HEAD's run: a pass that carries
/// the gating counters, else the run already stored for the binary. A pass
/// that skipped them must never be stored, or a later gate reusing it compares
/// nothing.
fn identical_pass_cache(stored: Option<&Value>, pass: &Run, counted: bool) -> Option<Value> {
    if counted {
        Some(ref_doc(pass))
    } else {
        stored.cloned()
    }
}

/// A run stored for the identical binary, its label, and the ids the pass
/// was asked to measure.
type StoredFor = Box<(Value, String, BTreeSet<String>)>;

/// HEAD's run from the identical-binaries pass. A pass without the gating
/// counters takes them from the run stored for the byte-identical binary.
fn identical_head(pass: Run, counted: bool, stored: Option<StoredFor>) -> (Run, HeadRun) {
    if counted {
        return (pass, HeadRun::Measured);
    }
    let Some((doc, from, expected)) = stored.map(|s| *s) else {
        return (pass, HeadRun::Uncounted);
    };
    match headrun::merge_stored(&doc, &from, pass, &expected) {
        Ok(run) => {
            println!("gate: counters for the identical binaries read from its stored run ({from})");
            (run, HeadRun::Measured)
        }
        Err(pass) => (*pass, HeadRun::Uncounted),
    }
}

/// The callgrind method the merge-base counts with, when it is not HEAD's.
/// `unknown` when it cannot be told: a comparison nobody could check is
/// inconclusive, not matched. `base` is `None` for identical binaries.
fn base_method_differs(
    head: &invoke::HostEnv,
    have_digests: bool,
    base: Option<&Result<invoke::HostEnv, String>>,
) -> Option<String> {
    if !have_digests {
        return Some("unknown".into());
    }
    match base? {
        Ok(base) if base.gating_backend != "callgrind" => None,
        Ok(base) => (base.method() != head.method()).then(|| base.method().to_string()),
        Err(_) => Some("unknown".into()),
    }
}

/// The head and merge-base bench binaries, with digests of their machine code.
struct Binaries {
    head: String,
    base: String,
    head_exe: std::path::PathBuf,
    base_exe: std::path::PathBuf,
}

/// Conservative: any build or extraction failure yields `None`, so the gate
/// falls through to a real measurement.
fn bench_text_digests(
    common: &CommonArgs,
    wt: &std::path::Path,
    wt_target: Option<&std::path::Path>,
) -> Option<Binaries> {
    let (Some(head), Some(base)) = (
        invoke::bench_executable(common, None, None),
        invoke::bench_executable(common, Some(wt), wt_target),
    ) else {
        return None;
    };
    // One path for both sides means extraction resolved head's own binary
    // twice; treat it as failed and fall through to a real measurement.
    if head == base {
        return None;
    }
    Some(Binaries {
        head: binary_digest(&head)?,
        base: binary_digest(&base)?,
        head_exe: head,
        base_exe: base,
    })
}

fn round_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

/// Fold a side's timing-only second round into its full first round. A
/// failed second round costs drift cancellation, not the gate: a merge-base
/// harness that predates `--skip-gating-counters` rejects the flag.
fn combine_rounds(first: Run, second: Result<Run, String>) -> Run {
    match second {
        Ok(r) => combine_min(first, r),
        Err(e) => {
            eprintln!("WARN: second timing round failed ({e}); using one round for this side");
            first
        }
    }
}

/// Per-metric minima of two rounds of the same side.
fn combine_min(mut first: Run, second: Run) -> Run {
    for (id, m2) in second.items {
        let m = first.items.entry(id).or_default();
        if let (Some(a), Some(b)) = (m.median_ns, m2.median_ns) {
            m.wall_rounds = vec![a, b];
        }
        m.median_ns = min_opt_f(m.median_ns, m2.median_ns);
        m.p99_ns = min_opt_f(m.p99_ns, m2.p99_ns);
        m.instructions = min_opt_u(m.instructions, m2.instructions);
        m.cycles = min_opt_u(m.cycles, m2.cycles);
        m.ir = min_opt_u(m.ir, m2.ir);
        m.allocs = min_opt_u(m.allocs, m2.allocs);
        m.bytes = min_opt_u(m.bytes, m2.bytes);
    }
    first
}

fn min_opt_f(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}
fn min_opt_u(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

/// Why a walltime overrun is a SOFT warning instead of a hard fail, if it is.
fn walltime_downgrade(old: &Value, cur: &ItemMetrics, limit: f64) -> Option<&'static str> {
    if counters_flat_on_unchanged_code(old, cur) {
        return Some("counters flat on unchanged code — environment, not regression");
    }
    if round_pairings_disagree(old, cur, limit) {
        return Some("not reproduced in both interleaved round pairings");
    }
    None
}

/// Whether this bench's already-accepted, unstale instructions/callgrind_ir
/// growth covers a walltime move proportionally: walltime softens up to the
/// accepted counter's own growth plus the normal walltime threshold, so a
/// slowdown that just tracks the accepted cost doesn't need its own review,
/// while a walltime-only pathology (lock contention, syscalls) past that
/// ceiling still fails.
fn accepted_counters_walltime_downgrade(
    entries: &gate_lock::Entries,
    id: &str,
    old: &Value,
    old_fp: &str,
    delta: f64,
    wall_limit: f64,
) -> Option<&'static str> {
    if gate_lock::is_stale(entries, id, old_fp) {
        return None;
    }
    let entry = entries.get(id)?;
    let ratio = |metric: &str, table: &str, field: &str| {
        let accepted = *entry.metrics.get(metric)?;
        let reference = old[table][field].as_f64().filter(|r| *r > 0.0)?;
        Some((accepted - reference) / reference * 100.0)
    };
    let accepted_ratio = match (
        ratio("instructions", "perfcnt", "instructions"),
        ratio("callgrind_ir", "callgrind", "ir"),
    ) {
        (Some(a), Some(b)) => a.max(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return None,
    };
    (delta <= accepted_ratio + wall_limit).then_some("counters already accepted for this bench")
}

/// A regression that exists only in the min-of-rounds comparison. One lucky
/// round on either side can manufacture that; a real slowdown shows up in
/// every pairing. Sides without two rounds keep the single comparison.
fn round_pairings_disagree(old: &Value, cur: &ItemMetrics, limit: f64) -> bool {
    let old_rounds: Vec<f64> = old["walltime"]["rounds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_f64)
        .collect();
    if old_rounds.len() != 2 || cur.wall_rounds.len() != 2 {
        return false;
    }
    let over = |o: f64, n: f64| o > 0.0 && (n - o) / o * 100.0 > limit;
    !(over(old_rounds[0], cur.wall_rounds[0]) && over(old_rounds[1], cur.wall_rounds[1]))
}

/// Whether the deterministic evidence contradicts a walltime regression:
/// unchanged fingerprint, at least one instruction-level counter present on
/// both sides, and every counter present on both sides flat. Identical code
/// doing identical work with an identical allocation profile cannot have
/// gotten slower — the clock was measuring the machine.
fn counters_flat_on_unchanged_code(old: &Value, cur: &ItemMetrics) -> bool {
    let fp = old["fingerprint"].as_str().unwrap_or("");
    if fp.is_empty() || fp != cur.fingerprint {
        return false;
    }
    let flat = |o: Option<u64>, n: Option<u64>| {
        let (o, n) = (o?, n?);
        if o == n {
            return Some(true);
        }
        if o == 0 {
            return Some(false);
        }
        let diff = o.abs_diff(n);
        Some(diff <= COUNTER_FLAT_ABS || diff as f64 / o as f64 * 100.0 <= COUNTER_FLAT_PCT)
    };
    let instructions = flat(old["perfcnt"]["instructions"].as_u64(), cur.instructions);
    let ir = flat(old["callgrind"]["ir"].as_u64(), cur.ir);
    if instructions.is_none() && ir.is_none() {
        return false; // no counters on both sides = no corroboration
    }
    let exact = |o: Option<u64>, n: Option<u64>| match (o, n) {
        (Some(o), Some(n)) => o == n,
        _ => true,
    };
    instructions != Some(false)
        && ir != Some(false)
        && exact(old["alloc"]["allocs"].as_u64(), cur.allocs)
        && exact(old["alloc"]["bytes"].as_u64(), cur.bytes)
}

/// How one comparison run should treat scope and disappearance.
#[derive(Clone)]
struct CompareCtx {
    deps_mode: bool,
    allow_gone: bool,
    filter: Option<String>,
    /// Items other shards own are out of scope, not GONE.
    shard: Option<invoke::Shard>,
    /// Package being gated (`-p`, raw name); baseline entries from other
    /// packages are out of scope, not GONE.
    pkg: Option<String>,
    buildcost_mode: bool,
    accepted: gate_lock::Entries,
    /// `gate accept --headroom PCT`: also collect near-miss metrics within
    /// `PCT` points of failing. `None` for every ordinary `gate` run.
    headroom_pct: Option<f64>,
    /// `[gate] walltime = "advisory"`: walltime overruns are SOFT.
    walltime_advisory: bool,
}

/// A metric that failed its check, collected only when the caller (`gate
/// accept`) needs to build a new lock entry from it.
struct FailedMetric {
    id: String,
    metric: String,
    new_value: f64,
}

/// Compare one reference document against the current run; returns failures,
/// records regressed item IDs for triage, and (for `gate accept`) collects
/// every metric that still fails after accepted overrides are applied, plus
/// (with `ctx.headroom_pct` set) every passing metric close enough to
/// failing to be worth recording in the same accept.
fn compare(
    reference: &Value,
    current: &Run,
    label: &str,
    ctx: &CompareCtx,
    failing_ids: &mut Vec<String>,
    out_failed: &mut Vec<FailedMetric>,
    out_near_miss: &mut Vec<FailedMetric>,
) -> u32 {
    let noise = current
        .noise_pct
        .unwrap_or(0.0)
        .max(reference["noise_pct"].as_f64().unwrap_or(0.0));
    let wall_limit = walltime_limit(noise);
    let build_drift = current
        .build
        .as_ref()
        .and_then(|b| buildstamp::compare(reference, b));
    if let Some(m) = &build_drift {
        println!(
            "{label}gate: {} — deterministic counters softened for this comparison",
            m.detail
        );
    }
    let build = current
        .build
        .as_ref()
        .map(|b| b.digest())
        .unwrap_or_else(|| "?".into());
    println!(
        "{label}gate: build={build} noise_floor={noise:.2}% thresholds: instructions +{INSTRUCTIONS_THRESHOLD_PCT}% ir +{IR_THRESHOLD_PCT}% walltime +{wall_limit:.1}% alloc/size +{ALLOC_THRESHOLD_PCT}% polls/wakes +{ASYNC_THRESHOLD_PCT}%"
    );
    if wall_limit > WALLTIME_THRESHOLD_PCT {
        println!(
            "NOTE: noise floor {noise:.2}% raised the walltime threshold to +{wall_limit:.1}% (deterministic metrics unaffected)"
        );
    }

    let build_soft = build_drift.as_ref().map(|m| m.reason.as_str());
    // A reused reference was read against another process's clock, so only
    // its counters carry over.
    let reused_soft = reference["reused_from"]
        .as_str()
        .map(|_| "reference measured in an earlier run");

    let old_items = &reference["items"];
    let mut failures = 0u32;
    let mut fingerprints_changed = false;
    let mut failed_here: Vec<String> = Vec::new();
    let mut failed_metrics: Vec<FailedMetric> = Vec::new();
    let mut near_miss_metrics: Vec<FailedMetric> = Vec::new();

    for (id, cur) in &current.items {
        let old = &old_items[id.as_str()];
        if old.is_null() {
            println!("NEW   {id} (no reference entry)");
            continue;
        }
        let old_fp = old["fingerprint"].as_str().unwrap_or("");
        if !old_fp.is_empty() && old_fp != cur.fingerprint {
            fingerprints_changed = true;
            println!("NOTE  {id}: code changed since reference");
        }

        if let (Some(was), Some(now)) = (old["callgrind"]["window"].as_u64(), cur.window)
            && was != now
        {
            println!("NOTE  {id}: callgrind ir averaged over {was} iterations, now {now}");
        }

        // Hard-gated relative metrics. A `downgrade` reason turns a hard
        // fail into a SOFT warning (walltime anti-flake rules). An accepted
        // entry — reviewed, one-sided ceiling anchored at its own value —
        // takes precedence over a plain FAIL, but not over a downgrade.
        let mut rel = |metric: &str,
                       old_v: Option<f64>,
                       new_v: Option<f64>,
                       limit: f64,
                       hard: bool,
                       noise_floor: f64,
                       downgrade: Option<&str>| {
            let (Some(o), Some(n)) = (old_v, new_v) else {
                return;
            };
            if o <= 0.0 {
                return;
            }
            let delta = (n - o) / o * 100.0;
            let fail = delta > limit;
            let accept = (fail && hard && downgrade.is_none())
                .then(|| gate_lock::allows(&ctx.accepted, id, metric, old_fp, n, limit))
                .flatten();
            let (verdict, why) = match (fail, hard, downgrade, accept) {
                (false, ..) => ("ok", None),
                (_, _, _, Some(true)) => ("ACPT", None),
                (true, true, None, _) => ("FAIL", None),
                (true, true, Some(why), _) => ("SOFT", Some(why)),
                (true, false, why, _) => ("SOFT", why),
            };
            if verdict == "FAIL" {
                failures += 1;
                failed_here.push(id.clone());
                failed_metrics.push(FailedMetric {
                    id: id.clone(),
                    metric: metric.into(),
                    new_value: n,
                });
            }
            if !fail
                && hard
                && let Some(headroom) = ctx.headroom_pct
                && delta > noise_floor
                && limit - delta <= headroom
            {
                near_miss_metrics.push(FailedMetric {
                    id: id.clone(),
                    metric: metric.into(),
                    new_value: n,
                });
            }
            let suffix = match (why, verdict) {
                (Some(w), _) => format!(" ({w})"),
                (None, "ACPT") => accept_suffix(&ctx.accepted, id),
                _ => String::new(),
            };
            println!("{verdict:<5} {label}{id} {metric} {o:.1} -> {n:.1} ({delta:+.1}%){suffix}");
            if verdict == "FAIL" && gate_lock::is_stale(&ctx.accepted, id, old_fp) {
                println!(
                    "NOTE  {label}{id}: accepted entry for {metric} is stale (base changed since accept) — re-run `gate accept`"
                );
            }
        };
        rel(
            "instructions",
            old["perfcnt"]["instructions"].as_u64().map(|v| v as f64),
            cur.instructions.map(|v| v as f64),
            counter_limit(INSTRUCTIONS_THRESHOLD_PCT, cur),
            true,
            HEADROOM_COUNTER_FLOOR_PCT,
            build_soft,
        );
        rel(
            "callgrind_ir",
            old["callgrind"]["ir"].as_u64().map(|v| v as f64),
            cur.ir.map(|v| v as f64),
            counter_limit(IR_THRESHOLD_PCT, cur),
            true,
            HEADROOM_COUNTER_FLOOR_PCT,
            build_soft,
        );
        let item_noise = noise.max(imported_noise_pct(old, cur));
        let wall_limit = walltime_limit(item_noise);
        if item_noise > noise && wall_limit > WALLTIME_THRESHOLD_PCT {
            println!(
                "NOTE  {label}{id}: noise floor {item_noise:.2}% raised the walltime threshold to +{wall_limit:.1}%"
            );
        }
        let walltime_delta = match (old["walltime"]["median_ns"].as_f64(), cur.median_ns) {
            (Some(o), Some(n)) if o > 0.0 => Some((n - o) / o * 100.0),
            _ => None,
        };
        let accepted_counters_soft = walltime_delta.and_then(|d| {
            accepted_counters_walltime_downgrade(&ctx.accepted, id, old, old_fp, d, wall_limit)
        });
        rel(
            "walltime_median_ns",
            old["walltime"]["median_ns"].as_f64(),
            cur.median_ns,
            counter_limit(wall_limit, cur),
            !ctx.walltime_advisory,
            WALLTIME_NOISE_MARGIN * item_noise,
            walltime_downgrade(old, cur, wall_limit)
                .or(build_soft)
                .or(reused_soft)
                .or((old["reused_from"].is_string() || cur.reused.is_some())
                    .then_some("item measured in an earlier run"))
                .or(accepted_counters_soft)
                .or(ctx
                    .walltime_advisory
                    .then_some("walltime is advisory in soothfast.toml")),
        );
        rel(
            "build_ms",
            old["buildcost"]["build_ms"].as_u64().map(|v| v as f64),
            cur.build_ms.map(|v| v as f64),
            BUILD_MS_SOFT_PCT,
            false,
            0.0,
            None,
        );

        // Integer metrics: old=0 allows nothing (zero-alloc stays zero-alloc).
        let mut int = |metric: &str, old_v: Option<u64>, new_v: Option<u64>, pct: u64| {
            let (Some(o), Some(n)) = (old_v, new_v) else {
                return;
            };
            let allowed = o + o * pct / 100;
            let fail = n > allowed;
            let accept = fail
                .then(|| gate_lock::allows(&ctx.accepted, id, metric, old_fp, n as f64, pct as f64))
                .flatten();
            let verdict = match (fail, accept) {
                (false, _) => "ok",
                (true, Some(true)) => "ACPT",
                _ => "FAIL",
            };
            if verdict == "FAIL" {
                failures += 1;
                failed_here.push(id.clone());
                failed_metrics.push(FailedMetric {
                    id: id.clone(),
                    metric: metric.into(),
                    new_value: n as f64,
                });
            }
            if !fail && let Some(headroom) = ctx.headroom_pct {
                let delta = if o > 0 {
                    (n as f64 - o as f64) / o as f64 * 100.0
                } else {
                    0.0
                };
                if delta > HEADROOM_COUNTER_FLOOR_PCT && pct as f64 - delta <= headroom {
                    near_miss_metrics.push(FailedMetric {
                        id: id.clone(),
                        metric: metric.into(),
                        new_value: n as f64,
                    });
                }
            }
            let suffix = if verdict == "ACPT" {
                accept_suffix(&ctx.accepted, id)
            } else {
                String::new()
            };
            println!("{verdict:<5} {label}{id} {metric} {o} -> {n} (allowed <= {allowed}){suffix}");
            if verdict == "FAIL" && gate_lock::is_stale(&ctx.accepted, id, old_fp) {
                println!(
                    "NOTE  {label}{id}: accepted entry for {metric} is stale (base changed since accept) — re-run `gate accept`"
                );
            }
        };
        int(
            "allocs",
            old["alloc"]["allocs"].as_u64(),
            cur.allocs,
            ALLOC_THRESHOLD_PCT,
        );
        int(
            "alloc_bytes",
            old["alloc"]["bytes"].as_u64(),
            cur.bytes,
            ALLOC_THRESHOLD_PCT,
        );
        int(
            "size_bytes",
            old["buildcost"]["size_bytes"].as_u64(),
            cur.size_bytes,
            SIZE_THRESHOLD_PCT,
        );
        int(
            "polls",
            old["asyncexec"]["polls"].as_u64(),
            cur.polls,
            ASYNC_THRESHOLD_PCT,
        );
        int(
            "wakes",
            old["asyncexec"]["wakes"].as_u64(),
            cur.wakes,
            ASYNC_THRESHOLD_PCT,
        );
    }
    if let Some(old_map) = old_items.as_object() {
        for id in old_map.keys() {
            if current.items.contains_key(id) {
                continue;
            }
            // Out-of-scope disappearances are expected: a --filter run only
            // measures matching ids, bench runs never see buildcost items,
            // and `-p PKG` never sees other packages' baseline entries.
            let filtered_out = ctx.filter.as_deref().is_some_and(|f| !id.contains(f))
                || ctx.shard.is_some_and(|s| !s.owns(id));
            let out_of_mode = id.starts_with("buildcost::") != ctx.buildcost_mode;
            // Bench ids start with the normalized package; buildcost ids
            // embed the raw one (`buildcost::PKG::combo`).
            let other_pkg =
                ctx.pkg
                    .as_deref()
                    .is_some_and(|p| match id.strip_prefix("buildcost::") {
                        Some(rest) => rest.split("::").next() != Some(p),
                        None => id.split("::").next() != Some(p.replace('-', "_").as_str()),
                    });
            if filtered_out || out_of_mode || other_pkg {
                continue;
            }
            if ctx.allow_gone {
                println!(
                    "GONE  {label}{id} (in reference, not measured now — allowed by --allow-gone)"
                );
            } else {
                // A vanished measured item must fail: deleting a regressed
                // bench (or losing its records) must not turn the gate green.
                failures += 1;
                println!(
                    "FAIL  {label}{id} GONE (in reference, not measured now; pass --allow-gone if intentional)"
                );
            }
        }
    }
    if ctx.deps_mode && fingerprints_changed {
        println!(
            "WARN: --deps given but item fingerprints changed — this is not a pure dependency bump"
        );
    }
    failing_ids.extend(failed_here);
    out_failed.extend(failed_metrics);
    out_near_miss.extend(near_miss_metrics);
    failures
}

/// The printed suffix naming the lock file and justification behind an
/// `ACPT` verdict, so a CI log reader can trace the waiver without opening
/// the repo.
fn accept_suffix(entries: &gate_lock::Entries, id: &str) -> String {
    gate_lock::latest_justification(entries, id)
        .map(|j| format!(" per {}: {j:?}", gate_lock::LOCKFILE))
        .unwrap_or_default()
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
        let exe = exe
            .map(std::path::Path::to_path_buf)
            .or_else(|| invoke::bench_executable(&g.common, None, None));
        let listed = exe.and_then(|exe| invoke::listed_ids(&exe, g.common.filter.as_deref()));
        errored_at(&root, name, &g.common, listed.as_deref(), msg);
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
    use super::{
        CompareCtx, FailedMetric, GateArgs, combine_min, combine_rounds, compare, identical_pass,
        identical_pass_args, identical_pass_cache, reuse_hint, round_duration, walltime_limit,
    };
    use crate::buildstamp;
    use crate::gate_lock;
    use crate::gate_save::save_head;
    use crate::invoke::{self, CommonArgs, HarnessSync, ItemMetrics, Run};
    use serde_json::{Value, json};

    fn ctx() -> CompareCtx {
        CompareCtx {
            deps_mode: false,
            allow_gone: false,
            filter: None,
            shard: None,
            pkg: None,
            buildcost_mode: false,
            accepted: gate_lock::Entries::new(),
            headroom_pct: None,
            walltime_advisory: false,
        }
    }

    fn reference(item: Value) -> Value {
        json!({ "version": 1, "noise_pct": 0.20, "items": { "pkg::bt_base_to_htf_index": item } })
    }

    fn run_with(m: ItemMetrics) -> Run {
        let mut run = Run::default();
        run.items.insert("pkg::bt_base_to_htf_index".into(), m);
        run
    }

    fn gate_failures(reference: &Value, current: &Run) -> (u32, Vec<String>) {
        gate_failures_with(reference, current, ctx())
    }

    fn gate_failures_with(reference: &Value, current: &Run, ctx: CompareCtx) -> (u32, Vec<String>) {
        let mut failing = Vec::new();
        let failures = compare(
            reference,
            current,
            "",
            &ctx,
            &mut failing,
            &mut Vec::new(),
            &mut Vec::new(),
        );
        (failures, failing)
    }

    fn stamp(profiles: &str) -> buildstamp::BuildStamp {
        buildstamp::BuildStamp {
            rustc: "1.88.0 x86_64-unknown-linux-gnu".into(),
            codegen_units: "1".into(),
            profiles: profiles.into(),
            rustflags: "0".into(),
        }
    }

    /// 321107 -> 337491 Ir is +5.1%, one instruction per element on a
    /// 16384-element workload: a repartitioned codegen unit, not a change.
    fn drift_reference(build: Option<Value>) -> Value {
        let mut doc = reference(json!({
            "fingerprint": "fp-unchanged",
            "perfcnt": { "instructions": 321107, "cycles": 91000, "cache_refs": 1200 },
        }));
        if let Some(b) = build {
            doc["build"] = b;
        }
        doc
    }

    fn drift_head(build: Option<buildstamp::BuildStamp>, tolerance: Option<f64>) -> Run {
        let mut run = run_with(ItemMetrics {
            fingerprint: "fp-unchanged".into(),
            instructions: Some(337491),
            tolerance_pct: tolerance,
            ..Default::default()
        });
        run.build = build;
        run
    }

    #[test]
    fn round_durations_read_as_minutes_and_seconds() {
        assert_eq!(
            round_duration(std::time::Duration::from_millis(45_900)),
            "45s"
        );
        assert_eq!(round_duration(std::time::Duration::from_secs(161)), "2m41s");
        assert_eq!(round_duration(std::time::Duration::from_secs(300)), "5m00s");
    }

    #[test]
    fn matching_build_stamps_still_fail_instructions() {
        let reference = drift_reference(Some(stamp("aaaa").to_json()));
        let (failures, failing) = gate_failures(&reference, &drift_head(Some(stamp("aaaa")), None));
        assert_eq!(failures, 1);
        assert_eq!(failing, ["pkg::bt_base_to_htf_index"]);
    }

    #[test]
    fn differing_build_stamps_soften_instructions() {
        let reference = drift_reference(Some(stamp("aaaa").to_json()));
        let (failures, failing) = gate_failures(&reference, &drift_head(Some(stamp("bbbb")), None));
        assert_eq!(failures, 0);
        assert!(failing.is_empty(), "a SOFT item must not enter triage");
    }

    #[test]
    fn reference_without_a_build_stamp_softens() {
        let (failures, failing) = gate_failures(
            &drift_reference(None),
            &drift_head(Some(stamp("aaaa")), None),
        );
        assert_eq!(failures, 0);
        assert!(failing.is_empty());
    }

    #[test]
    fn tolerance_widens_the_instruction_threshold() {
        let reference = drift_reference(Some(stamp("aaaa").to_json()));
        let head = drift_head(Some(stamp("aaaa")), Some(8.0));
        assert_eq!(gate_failures(&reference, &head).0, 0);
    }

    #[test]
    fn tolerance_below_the_delta_still_fails() {
        let reference = drift_reference(Some(stamp("aaaa").to_json()));
        let head = drift_head(Some(stamp("aaaa")), Some(2.0));
        assert_eq!(gate_failures(&reference, &head).0, 1);
    }

    fn accepted_entries(base_fingerprint: &str, metric: &str, value: f64) -> gate_lock::Entries {
        let mut entries = gate_lock::Entries::new();
        entries.insert(
            "pkg::bt_base_to_htf_index".into(),
            gate_lock::Entry {
                base_fingerprint: base_fingerprint.into(),
                head_fingerprint: "fp-head".into(),
                metrics: [(metric.to_string(), value)].into_iter().collect(),
                justifications: vec![gate_lock::Justification {
                    text: "reviewed bug fix".into(),
                    accepted_unix: 1,
                }],
            },
        );
        entries
    }

    #[test]
    fn an_accepted_entry_suppresses_a_matching_fail() {
        let reference = drift_reference(Some(stamp("aaaa").to_json()));
        let head = drift_head(Some(stamp("aaaa")), None);
        let ctx = CompareCtx {
            accepted: accepted_entries("fp-unchanged", "instructions", 337491.0),
            ..ctx()
        };
        let (failures, failing) = gate_failures_with(&reference, &head, ctx);
        assert_eq!(failures, 0);
        assert!(failing.is_empty(), "an ACPT item must not enter triage");
    }

    #[test]
    fn a_stale_accepted_entry_still_fails() {
        let reference = drift_reference(Some(stamp("aaaa").to_json()));
        let head = drift_head(Some(stamp("aaaa")), None);
        let ctx = CompareCtx {
            accepted: accepted_entries("fp-old-base", "instructions", 337491.0),
            ..ctx()
        };
        assert_eq!(gate_failures_with(&reference, &head, ctx).0, 1);
    }

    #[test]
    fn a_value_well_below_the_accepted_ceiling_still_passes() {
        // One-sided: the accept bounds an upper ceiling, not a band, so a
        // value far under the accepted number needs no separate re-review.
        let reference = drift_reference(Some(stamp("aaaa").to_json()));
        let head = drift_head(Some(stamp("aaaa")), None);
        let ctx = CompareCtx {
            accepted: accepted_entries("fp-unchanged", "instructions", 500000.0),
            ..ctx()
        };
        assert_eq!(gate_failures_with(&reference, &head, ctx).0, 0);
    }

    #[test]
    fn exceeding_the_accepted_ceiling_still_fails() {
        let reference = drift_reference(Some(stamp("aaaa").to_json()));
        let mut head = drift_head(Some(stamp("aaaa")), None);
        head.items
            .get_mut("pkg::bt_base_to_htf_index")
            .unwrap()
            .instructions = Some(1_000_000);
        let ctx = CompareCtx {
            accepted: accepted_entries("fp-unchanged", "instructions", 337491.0),
            ..ctx()
        };
        assert_eq!(gate_failures_with(&reference, &head, ctx).0, 1);
    }

    fn walltime_accept_reference() -> Value {
        reference(json!({
            "fingerprint": "fp-unchanged",
            "perfcnt": { "instructions": 100000 },
            "walltime": { "median_ns": 1000.0, "mad_ns": 5.0, "p99_ns": 1100.0 },
        }))
    }

    #[test]
    fn headroom_collects_a_near_miss_metric_within_range() {
        let reference = drift_reference(Some(stamp("aaaa").to_json()));
        let mut head = drift_head(Some(stamp("aaaa")), None);
        // 321107 -> 336520 is +4.8%: under the 5% limit, within 1pt headroom
        // of it, and past the 1.5pt noise floor.
        head.items
            .get_mut("pkg::bt_base_to_htf_index")
            .unwrap()
            .instructions = Some(336520);
        let ctx = CompareCtx {
            headroom_pct: Some(1.0),
            ..ctx()
        };
        let mut out_failed = Vec::new();
        let mut out_near_miss = Vec::new();
        compare(
            &reference,
            &head,
            "",
            &ctx,
            &mut Vec::new(),
            &mut out_failed,
            &mut out_near_miss,
        );
        assert!(out_failed.is_empty());
        assert_eq!(out_near_miss.len(), 1);
        assert_eq!(out_near_miss[0].metric, "instructions");
    }

    #[test]
    fn headroom_excludes_a_delta_within_range_but_under_the_noise_floor() {
        let reference = drift_reference(Some(stamp("aaaa").to_json()));
        let mut head = drift_head(Some(stamp("aaaa")), None);
        // 321107 -> 324318 is +1.0%: within a wide 4.5pt headroom of the 5%
        // limit, but under the 1.5pt noise floor.
        head.items
            .get_mut("pkg::bt_base_to_htf_index")
            .unwrap()
            .instructions = Some(324318);
        let ctx = CompareCtx {
            headroom_pct: Some(4.5),
            ..ctx()
        };
        let mut out_near_miss = Vec::new();
        compare(
            &reference,
            &head,
            "",
            &ctx,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut out_near_miss,
        );
        assert!(out_near_miss.is_empty());
    }

    #[test]
    fn headroom_is_off_by_default() {
        let reference = drift_reference(Some(stamp("aaaa").to_json()));
        let mut head = drift_head(Some(stamp("aaaa")), None);
        head.items
            .get_mut("pkg::bt_base_to_htf_index")
            .unwrap()
            .instructions = Some(336520);
        let mut out_near_miss = Vec::new();
        compare(
            &reference,
            &head,
            "",
            &ctx(),
            &mut Vec::new(),
            &mut Vec::new(),
            &mut out_near_miss,
        );
        assert!(out_near_miss.is_empty());
    }

    #[test]
    fn accepted_counters_soften_walltime_proportionally() {
        let reference = walltime_accept_reference();
        // +25% walltime, within the 17% accepted instructions ratio plus
        // the 10% normal walltime threshold (27% ceiling).
        let head = run_with(ItemMetrics {
            fingerprint: "fp-unchanged".into(),
            median_ns: Some(1250.0),
            ..ItemMetrics::default()
        });
        let ctx = CompareCtx {
            accepted: accepted_entries("fp-unchanged", "instructions", 117000.0),
            ..ctx()
        };
        let (failures, failing) = gate_failures_with(&reference, &head, ctx);
        assert_eq!(failures, 0);
        assert!(failing.is_empty(), "a SOFT item must not enter triage");
    }

    #[test]
    fn walltime_still_fails_past_the_accepted_ceiling() {
        let reference = walltime_accept_reference();
        // +30% walltime, past the 27% ceiling: a real wall-clock pathology
        // the accepted counter growth alone doesn't explain.
        let head = run_with(ItemMetrics {
            fingerprint: "fp-unchanged".into(),
            median_ns: Some(1300.0),
            ..ItemMetrics::default()
        });
        let ctx = CompareCtx {
            accepted: accepted_entries("fp-unchanged", "instructions", 117000.0),
            ..ctx()
        };
        assert_eq!(gate_failures_with(&reference, &head, ctx).0, 1);
    }

    #[test]
    fn out_failed_collects_failing_metrics_for_accept() {
        let reference = drift_reference(Some(stamp("aaaa").to_json()));
        let head = drift_head(Some(stamp("aaaa")), None);
        let mut failing = Vec::new();
        let mut out: Vec<FailedMetric> = Vec::new();
        compare(
            &reference,
            &head,
            "",
            &ctx(),
            &mut failing,
            &mut out,
            &mut Vec::new(),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "pkg::bt_base_to_htf_index");
        assert_eq!(out[0].metric, "instructions");
        assert_eq!(out[0].new_value, 337491.0);
    }

    #[test]
    fn out_failed_is_empty_when_accepted() {
        let reference = drift_reference(Some(stamp("aaaa").to_json()));
        let head = drift_head(Some(stamp("aaaa")), None);
        let ctx = CompareCtx {
            accepted: accepted_entries("fp-unchanged", "instructions", 337491.0),
            ..ctx()
        };
        let mut failing = Vec::new();
        let mut out: Vec<FailedMetric> = Vec::new();
        compare(
            &reference,
            &head,
            "",
            &ctx,
            &mut failing,
            &mut out,
            &mut Vec::new(),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_regression_against_a_reused_perfcnt_reference_names_the_recovery() {
        let reused = json!({ "reused_from": "1aa6c4de", "items": {} });
        assert_eq!(
            reuse_hint(&reused, Some("perfcnt"), 1).as_deref(),
            Some("gate: reference reused from 1aa6c4de; rerun with --no-reuse-base to re-measure")
        );
    }

    #[test]
    fn no_reuse_hint_without_a_reused_perfcnt_regression() {
        let reused = json!({ "reused_from": "1aa6c4de", "items": {} });
        assert_eq!(reuse_hint(&reused, Some("perfcnt"), 0), None);
        assert_eq!(reuse_hint(&reused, Some("callgrind"), 1), None);
        assert_eq!(
            reuse_hint(&json!({ "items": {} }), Some("perfcnt"), 1),
            None
        );
    }

    #[test]
    fn a_named_gating_backend_keeps_its_counters_on_the_identical_pass() {
        assert!(identical_pass_args(Some("perfcnt"), false).is_empty());
        assert!(identical_pass_args(Some("callgrind"), false).is_empty());
    }

    #[test]
    fn the_identical_pass_skips_counters_otherwise() {
        assert_eq!(identical_pass_args(None, false), ["--skip-gating-counters"]);
        assert_eq!(
            identical_pass_args(Some("walltime"), false),
            ["--skip-gating-counters"]
        );
    }

    #[test]
    fn only_a_counted_identical_pass_is_stored_afresh() {
        let pass = Run::default();
        assert!(identical_pass_cache(None, &pass, false).is_none());
        assert!(identical_pass_cache(None, &pass, true).is_some());
    }

    #[test]
    fn a_stored_run_is_kept_when_the_pass_is_not_counted() {
        let stored = json!({ "version": 1, "items": {} });
        assert_eq!(
            identical_pass_cache(Some(&stored), &Run::default(), false),
            Some(stored)
        );
    }

    #[test]
    fn the_identical_branch_plans_whenever_reuse_is_on() {
        for (stored, recording) in [(false, false), (true, false), (false, true), (true, true)] {
            assert!(identical_pass(true, stored, recording, None).plan);
        }
        assert!(!identical_pass(false, false, false, None).plan);
    }

    #[test]
    fn a_plan_that_applies_counts_and_one_that_does_not_takes_the_cheap_pass() {
        let pass = identical_pass(true, false, false, None);
        assert!(pass.counted(true));
        assert!(!pass.counted(false));
        assert_eq!(pass.args, ["--skip-gating-counters"]);
    }

    #[test]
    fn a_stored_run_keeps_the_cheap_fallback_even_when_recording() {
        let pass = identical_pass(false, true, true, None);
        assert_eq!(pass.args, ["--skip-gating-counters"]);
        assert!(!pass.counted(false));
    }

    #[test]
    fn a_recording_fallback_counts_and_is_baseline_material() {
        let pass = identical_pass(false, false, true, Some("walltime"));
        assert!(pass.args.is_empty());
        assert!(pass.counted(false));
    }

    #[test]
    fn a_counted_identical_run_saves_the_baseline() {
        assert!(identical_pass(true, true, false, None).counted(true));
    }

    #[test]
    fn a_recording_identical_pass_keeps_the_counters() {
        assert!(identical_pass_args(None, true).is_empty());
        assert!(identical_pass_args(Some("walltime"), true).is_empty());
    }

    fn reused(mut doc: Value) -> Value {
        doc["reused_from"] = json!("deadbeef");
        doc
    }

    fn walltime_reference() -> Value {
        reference(json!({
            "fingerprint": "fp-base",
            "walltime": { "median_ns": 1000.0, "mad_ns": 5.0, "p99_ns": 1100.0 },
        }))
    }

    fn walltime_head() -> Run {
        run_with(ItemMetrics {
            fingerprint: "fp-head".into(),
            median_ns: Some(1300.0),
            ..Default::default()
        })
    }

    #[test]
    fn a_freshly_measured_reference_fails_walltime() {
        assert_eq!(gate_failures(&walltime_reference(), &walltime_head()).0, 1);
    }

    #[test]
    fn a_reused_reference_softens_walltime() {
        let (failures, failing) = gate_failures(&reused(walltime_reference()), &walltime_head());
        assert_eq!(failures, 0);
        assert!(failing.is_empty());
    }

    #[test]
    fn a_vanished_item_fails_only_in_the_shard_that_owns_it() {
        let gone =
            json!({ "version": 1, "items": { "pkg::gone": {}, "pkg::bt_base_to_htf_index": {} } });
        let head = run_with(ItemMetrics::default());
        let failures: u32 = (1..=3)
            .map(|index| {
                let ctx = CompareCtx {
                    shard: Some(crate::invoke::Shard { index, count: 3 }),
                    ..ctx()
                };
                gate_failures_with(&gone, &head, ctx).0
            })
            .sum();
        assert_eq!(failures, 1);
    }

    #[test]
    fn an_item_reused_from_the_store_softens_its_walltime() {
        let mut doc = walltime_reference();
        doc["items"]["pkg::bt_base_to_htf_index"]["reused_from"] = json!("item cache");
        assert_eq!(gate_failures(&doc, &walltime_head()).0, 0);
        let mut head = walltime_head();
        for item in head.items.values_mut() {
            item.reused = Some(crate::invoke::Reused {
                from: "item cache".into(),
                metrics: Vec::new(),
            });
        }
        assert_eq!(gate_failures(&walltime_reference(), &head).0, 0);
    }

    #[test]
    fn advisory_walltime_never_fails() {
        let ctx = CompareCtx {
            walltime_advisory: true,
            ..ctx()
        };
        let (failures, failing) = gate_failures_with(&walltime_reference(), &walltime_head(), ctx);
        assert_eq!(failures, 0);
        assert!(failing.is_empty(), "a SOFT item must not enter triage");
    }

    #[test]
    fn advisory_walltime_still_fails_counters() {
        let ctx = CompareCtx {
            walltime_advisory: true,
            ..ctx()
        };
        let mut head = incident_head();
        head.instructions = Some(400_000);
        let (failures, failing) =
            gate_failures_with(&incident_reference("fp-old"), &run_with(head), ctx);
        assert_eq!(failures, 1, "instructions still gate; walltime is SOFT");
        assert_eq!(failing, vec!["pkg::bt_base_to_htf_index"]);
    }

    #[test]
    fn a_reused_reference_still_fails_instructions() {
        let doc = reused(drift_reference(Some(stamp("aaaa").to_json())));
        let head = drift_head(Some(stamp("aaaa")), None);
        assert_eq!(
            gate_failures(&doc, &head).0,
            1,
            "reuse must not soften counters"
        );
    }

    /// The motivating incident: walltime 22995 -> 27327 ns (+18.8%) with
    /// byte-identical instructions, identical allocs, unchanged fingerprint.
    fn incident_reference(fingerprint: &str) -> Value {
        reference(json!({
            "fingerprint": fingerprint,
            "walltime": { "median_ns": 22995.0, "mad_ns": 46.0, "p99_ns": 23500.0 },
            "perfcnt": { "instructions": 337491, "cycles": 91000, "cache_refs": 1200 },
            "alloc": { "allocs": 57, "bytes": 4096 },
        }))
    }

    fn incident_head() -> ItemMetrics {
        ItemMetrics {
            fingerprint: "fp-unchanged".into(),
            median_ns: Some(27327.0),
            instructions: Some(337491),
            allocs: Some(57),
            bytes: Some(4096),
            ..ItemMetrics::default()
        }
    }

    #[test]
    fn flat_counters_on_unchanged_code_downgrade_a_walltime_fail() {
        let (failures, failing) = gate_failures(
            &incident_reference("fp-unchanged"),
            &run_with(incident_head()),
        );
        assert_eq!(failures, 0);
        assert!(failing.is_empty(), "a SOFT item must not enter triage");
    }

    #[test]
    fn a_walltime_fail_on_changed_code_still_fails() {
        let (failures, failing) =
            gate_failures(&incident_reference("fp-old"), &run_with(incident_head()));
        assert_eq!(failures, 1);
        assert_eq!(failing, vec!["pkg::bt_base_to_htf_index"]);
    }

    fn rounds_reference(rounds: [f64; 2]) -> Value {
        reference(json!({
            "fingerprint": "fp-old",
            "walltime": { "median_ns": 22995.0, "mad_ns": 46.0, "p99_ns": 23500.0,
                          "rounds": rounds },
            "perfcnt": { "instructions": 337491 },
        }))
    }

    fn rounds_head(rounds: [f64; 2]) -> ItemMetrics {
        ItemMetrics {
            fingerprint: "fp-new".into(),
            median_ns: Some(rounds[0].min(rounds[1])),
            instructions: Some(337491),
            wall_rounds: rounds.to_vec(),
            ..ItemMetrics::default()
        }
    }

    #[test]
    fn a_single_lucky_base_round_downgrades_a_walltime_fail() {
        // Base round 1 is anomalously fast; pairing 2 shows no regression.
        let (failures, failing) = gate_failures(
            &rounds_reference([22995.0, 27200.0]),
            &run_with(rounds_head([27327.0, 27350.0])),
        );
        assert_eq!(failures, 0);
        assert!(failing.is_empty());
    }

    #[test]
    fn a_regression_in_both_pairings_still_fails() {
        let (failures, _) = gate_failures(
            &rounds_reference([22995.0, 23100.0]),
            &run_with(rounds_head([27327.0, 27350.0])),
        );
        assert_eq!(failures, 1);
    }

    #[test]
    fn a_side_with_one_round_keeps_the_single_comparison() {
        let mut head = rounds_head([27327.0, 27350.0]);
        head.wall_rounds.clear();
        let (failures, _) = gate_failures(&rounds_reference([22995.0, 27200.0]), &run_with(head));
        assert_eq!(failures, 1);
    }

    #[test]
    fn combine_min_records_both_rounds() {
        let m = |ns: f64| ItemMetrics {
            median_ns: Some(ns),
            ..ItemMetrics::default()
        };
        let mut a = Run::default();
        a.items.insert("pkg::x".into(), m(100.0));
        let mut b = Run::default();
        b.items.insert("pkg::x".into(), m(90.0));
        let merged = combine_min(a, b);
        let item = &merged.items["pkg::x"];
        assert_eq!(item.wall_rounds, vec![100.0, 90.0]);
        assert_eq!(item.median_ns, Some(90.0));
    }

    #[test]
    fn a_walltime_fail_with_moving_counters_still_fails() {
        // +3.7% instructions passes its own +5% gate but breaks the
        // "identical work" corroboration — the slowdown could be real.
        let head = ItemMetrics {
            instructions: Some(350_000),
            ..incident_head()
        };
        let (failures, _) = gate_failures(&incident_reference("fp-unchanged"), &run_with(head));
        assert_eq!(failures, 1);
    }

    #[test]
    fn a_walltime_fail_with_changed_allocs_still_fails() {
        let head = ItemMetrics {
            allocs: Some(58),
            ..incident_head()
        };
        let (failures, _) = gate_failures(&incident_reference("fp-unchanged"), &run_with(head));
        assert_eq!(failures, 1);
    }

    #[test]
    fn no_instruction_counters_means_no_corroboration() {
        let old = reference(json!({
            "fingerprint": "fp-unchanged",
            "walltime": { "median_ns": 22995.0, "mad_ns": 46.0, "p99_ns": 23500.0 },
            "alloc": { "allocs": 57, "bytes": 4096 },
        }));
        let head = ItemMetrics {
            instructions: None,
            ..incident_head()
        };
        let (failures, _) = gate_failures(&old, &run_with(head));
        assert_eq!(failures, 1);
    }

    /// A ~20K-instruction benchmark's allocator/dynamic-linker churn moves
    /// the count past 0.5% while fingerprint/allocs/bytes stay identical.
    fn issue93_reference() -> Value {
        reference(json!({
            "fingerprint": "fp-unchanged",
            "walltime": { "median_ns": 1313.2, "mad_ns": 5.0, "p99_ns": 1400.0 },
            "perfcnt": { "instructions": 19965 },
            "alloc": { "allocs": 12, "bytes": 512 },
        }))
    }

    fn issue93_head(instructions: u64) -> ItemMetrics {
        ItemMetrics {
            fingerprint: "fp-unchanged".into(),
            median_ns: Some(1823.0),
            instructions: Some(instructions),
            allocs: Some(12),
            bytes: Some(512),
            ..ItemMetrics::default()
        }
    }

    #[test]
    fn small_benchmark_instruction_noise_downgrades_a_walltime_fail() {
        // 19965 -> 19859 is -0.53%, past COUNTER_FLAT_PCT alone, but the
        // 106-instruction move is under the absolute floor for a bench this size.
        let (failures, failing) =
            gate_failures(&issue93_reference(), &run_with(issue93_head(19859)));
        assert_eq!(failures, 0);
        assert!(failing.is_empty(), "a SOFT item must not enter triage");
    }

    #[test]
    fn small_benchmark_floor_does_not_mask_a_real_instruction_change() {
        // 19965 -> 19645 is a 320-instruction, -1.60% move: past both the
        // absolute floor and COUNTER_FLAT_PCT, so it must still corroborate a fail.
        let (failures, _) = gate_failures(&issue93_reference(), &run_with(issue93_head(19645)));
        assert_eq!(failures, 1);
    }

    /// Gate a new crate whose one alloc claim holds or not, saving into `root`.
    fn gate_new_crate(root: &std::path::Path, holds: bool) -> u32 {
        let mut head = run_with(ItemMetrics {
            allocs: Some(0),
            bytes: Some(0),
            ..ItemMetrics::default()
        });
        head.assertions.push(invoke::AssertionOutcome {
            id: "pkg::bt_base_to_htf_index".into(),
            kind: "alloc".into(),
            ok: holds,
            detail: "claimed 0".into(),
        });
        let resolved = super::self_compared(head);
        let (regressions, _) = gate_failures(&resolved.reference, &resolved.current);
        let failures = regressions + super::assertion_failures(&resolved.current, &mut Vec::new());
        let common = CommonArgs {
            pkg: Some("pkg".into()),
            ..CommonArgs::default()
        };
        let current = &resolved.current;
        save_head(
            root,
            "head",
            current,
            &resolved.head,
            &common,
            None,
            failures,
        )
        .unwrap();
        failures
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("soothfast-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn a_passing_new_crate_saves_its_run() {
        let root = scratch("new-crate-pass");
        assert_eq!(gate_new_crate(&root, true), 0);
        let saved = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
        assert_eq!(
            saved["items"]["pkg::bt_base_to_htf_index"]["alloc"]["allocs"],
            0
        );
        assert_eq!(saved["assertions"][0]["ok"], true);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_uncounted_identical_pass_takes_the_identical_binary_s_stored_counters() {
        use crate::gate_save::HeadRun;
        let pass = || {
            run_with(ItemMetrics {
                median_ns: Some(5.0),
                ..ItemMetrics::default()
            })
        };
        let stored =
            json!({ "items": { "pkg::bt_base_to_htf_index": { "callgrind": { "ir": 7 } } } });
        let ids = std::collections::BTreeSet::from(["pkg::bt_base_to_htf_index".to_string()]);
        let stored = Box::new((stored, "binary abc".to_string(), ids));
        let (run, source) = super::identical_head(pass(), false, Some(stored));
        assert_eq!(source, HeadRun::Measured);
        assert_eq!(run.items["pkg::bt_base_to_htf_index"].ir, Some(7));
        assert_eq!(run.items["pkg::bt_base_to_htf_index"].median_ns, Some(5.0));
        assert_eq!(run.reused.unwrap().from, "binary abc");
        let (_, source) = super::identical_head(pass(), false, None);
        assert_eq!(source, HeadRun::Uncounted);
    }

    #[test]
    fn a_gate_whose_bench_did_not_build_covers_its_package_for_unchanged_claims_only() {
        let root = scratch("bench-unbuilt");
        let stale = json!({
            "version": 1,
            "items": { "pkg::old": { "alloc": { "allocs": 1, "bytes": 8 } } },
        });
        invoke::write_baseline_in(&root, "head", &stale).unwrap();
        let common = CommonArgs {
            pkg: Some("pkg".into()),
            ..CommonArgs::default()
        };
        super::errored_at(&root, "head", &common, None, "cargo bench failed");
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
        });
        invoke::write_baseline_in(&root, "head", &stale).unwrap();
        let e = super::ratchet_in(&root, "release").unwrap_err();
        let common = CommonArgs {
            pkg: Some("pkg".into()),
            ..CommonArgs::default()
        };
        let listed = ["pkg::new".to_string()];
        super::errored_at(&root, "head", &common, Some(&listed), &e);
        let saved = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
        assert_eq!(saved["items"], json!({}));
        assert_eq!(saved["unmeasured"][0]["reason"], "gate-failed");
        assert_eq!(saved["unmeasured"][0]["items"], json!(["pkg::new"]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failing_new_crate_fails_the_gate_and_saves_no_numbers() {
        let root = scratch("new-crate-fail");
        assert_eq!(gate_new_crate(&root, false), 1);
        let saved = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
        assert_eq!(saved["items"], json!({}));
        assert_eq!(saved["unmeasured"][0]["reason"], "gate-failed");
        assert_eq!(saved["unmeasured"][0]["package"], "pkg");
        assert_eq!(saved["unmeasured"][0]["all_items"], true);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn quiet_runners_keep_the_fixed_threshold() {
        assert_eq!(walltime_limit(0.0), 10.0);
        assert_eq!(walltime_limit(0.22), 10.0);
        assert_eq!(walltime_limit(3.33), 10.0);
    }

    #[test]
    fn noisy_runners_scale_instead_of_dropping_walltime() {
        assert_eq!(walltime_limit(4.75), 14.25);
        // Clears the A/A control's worst observed false positive (+11.8%).
        assert!(walltime_limit(4.75) > 11.8);
    }

    #[test]
    fn combine_min_keeps_counters_a_timing_only_round_lacks() {
        let mut full = Run {
            noise_pct: Some(1.5),
            gating_backend: Some("callgrind".into()),
            ..Run::default()
        };
        full.items.insert(
            "pkg::item".into(),
            ItemMetrics {
                ir: Some(100),
                instructions: Some(200),
                median_ns: Some(50.0),
                allocs: Some(3),
                ..ItemMetrics::default()
            },
        );
        let mut timing_only = Run::default();
        timing_only.items.insert(
            "pkg::item".into(),
            ItemMetrics {
                median_ns: Some(40.0),
                allocs: Some(3),
                ..ItemMetrics::default()
            },
        );

        let merged = combine_min(full, timing_only);
        let m = &merged.items["pkg::item"];
        assert_eq!(m.ir, Some(100));
        assert_eq!(m.instructions, Some(200));
        assert_eq!(m.median_ns, Some(40.0));
        assert_eq!(m.allocs, Some(3));
        // Calibration and env records ride on round 1, the full one.
        assert_eq!(merged.noise_pct, Some(1.5));
        assert_eq!(merged.gating_backend.as_deref(), Some("callgrind"));
    }

    #[test]
    fn a_failed_second_round_degrades_to_the_full_first_round() {
        let mut full = Run::default();
        full.items.insert(
            "pkg::item".into(),
            ItemMetrics {
                ir: Some(100),
                ..ItemMetrics::default()
            },
        );
        let merged = combine_rounds(full, Err("unknown runner arg".into()));
        assert_eq!(merged.items["pkg::item"].ir, Some(100));
    }

    fn mismatched() -> HarnessSync {
        HarnessSync::from_unpinned(vec!["soothfast 0.2.0 -> 0.3.1".into()])
    }

    #[test]
    fn a_regression_on_a_mismatched_harness_names_the_flag_that_waives_it() {
        let text = super::harness_verdict(&mismatched(), true);
        assert!(text.contains("--allow-harness-change"));
        assert!(text.contains("soothfast 0.2.0 -> 0.3.1"));
    }

    #[test]
    fn a_clean_run_on_a_mismatched_harness_still_says_so() {
        let text = super::harness_verdict(&mismatched(), false);
        assert!(text.contains("different harness"));
        assert!(text.contains("soothfast 0.2.0 -> 0.3.1"));
        assert!(!text.contains("--allow-harness-change"));
        assert!(super::harness_verdict(&HarnessSync::default(), false).is_empty());
    }

    #[test]
    fn the_flag_waives_a_regression_and_nothing_else() {
        assert!(super::harness_waiver(&mismatched(), true, 1));
        assert!(!super::harness_waiver(&mismatched(), false, 1));
        // A pinned reference means a regression is the code's, flag or not.
        assert!(!super::harness_waiver(&HarnessSync::default(), true, 1));
        // Nothing regressed, so there is nothing to waive.
        assert!(!super::harness_waiver(&mismatched(), true, 0));
    }

    #[test]
    fn a_reference_served_from_the_cache_keeps_its_mismatch() {
        let mut doc = json!({ "version": 1, "items": {} });
        super::mark_harness(&mut doc, &mismatched());
        assert_eq!(super::harness_of(&doc), mismatched());
        let clean = json!({ "version": 1, "items": {} });
        assert_eq!(super::harness_of(&clean), HarnessSync::default());
    }

    #[test]
    fn a_stored_reference_is_only_marked_when_the_pin_missed() {
        let mut doc = json!({ "version": 1, "items": {} });
        super::mark_harness(&mut doc, &HarnessSync::default());
        assert_eq!(doc, json!({ "version": 1, "items": {} }));
    }

    #[test]
    fn the_triage_artifact_carries_the_mismatch_and_drops_it_again() {
        let dir = std::env::temp_dir().join("soothfast-test-triage-harness");
        let _ = std::fs::remove_dir_all(&dir);
        super::triage_harness_note(&dir, &mismatched());
        let note = std::fs::read_to_string(dir.join("harness-mismatch.txt")).unwrap();
        assert!(note.contains("soothfast 0.2.0 -> 0.3.1"));
        super::triage_harness_note(&dir, &HarnessSync::default());
        assert!(!dir.join("harness-mismatch.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
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
            import: super::ImportArgs {
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
        let code = super::run_imported(&g, &root);
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

    fn callgrind_env(method: Option<&str>) -> invoke::HostEnv {
        invoke::HostEnv {
            gating_backend: "callgrind".into(),
            guest: None,
            callgrind_method: method.map(String::from),
            item_reuse: true,
        }
    }

    #[test]
    fn a_merge_base_that_counts_another_way_or_cannot_say_is_a_mismatch() {
        let head = callgrind_env(Some("one-process"));
        let differs = |have, base| super::base_method_differs(&head, have, base);
        let two = Ok(callgrind_env(None));
        let one = Ok(callgrind_env(Some("one-process")));
        assert_eq!(differs(true, Some(&two)).as_deref(), Some("two-process"));
        assert_eq!(differs(true, Some(&one)), None);
        assert_eq!(differs(true, None), None);
        assert_eq!(differs(false, None).as_deref(), Some("unknown"));
        assert_eq!(
            differs(true, Some(&Err("no".into()))).as_deref(),
            Some("unknown")
        );
    }

    #[test]
    fn a_merge_base_that_does_not_gate_on_callgrind_is_not_a_method_mismatch() {
        let head = callgrind_env(Some("one-process"));
        let mut walltime = callgrind_env(None);
        walltime.gating_backend = "walltime".into();
        assert_eq!(
            super::base_method_differs(&head, true, Some(&Ok(walltime))),
            None
        );
    }
}
