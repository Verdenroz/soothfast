//! The regression gate: re-measure, compare against a stored baseline (or a
//! merge-base worktree with `--against-ref`) with per-metric thresholds,
//! exit non-zero on regression. On failure, emits a callgrind triage
//! artifact when valgrind is available.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::buildstamp;
use crate::gate_config;
use crate::gate_lock;
use crate::invoke::{self, CommonArgs, ItemMetrics, Run};
use crate::runcache;
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
const COUNTER_FLAT_PCT: f64 = 0.5;
/// Absolute floor alongside `COUNTER_FLAT_PCT`: below ~30K instructions the
/// 0.5% cutoff is tighter than perfcnt's own run-to-run wobble.
const COUNTER_FLAT_ABS: u64 = 150;

/// Effective walltime threshold: the fixed +10%, raised to 3x the measured
/// A/A noise floor when that is higher. A fixed threshold is ~2 sigma on a
/// noisy runner — several false positives per run across a few hundred
/// items — while dropping walltime entirely would leave such runners
/// unmonitored. Scaling keeps every run gated at ≥3 sigma.
fn walltime_limit(noise_pct: f64) -> f64 {
    WALLTIME_THRESHOLD_PCT.max(noise_pct * WALLTIME_NOISE_MARGIN)
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
    matrix: String,
    save_baseline: Option<String>,
    reuse_base: bool,
}

pub fn run(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("accept") {
        return accept_cmd(&args[1..]);
    }
    let mut g = GateArgs {
        common: CommonArgs::default(),
        baseline: "base".into(),
        ratchet: None,
        against_ref: None,
        deps: false,
        allow_gone: false,
        matrix: "default".into(),
        save_baseline: None,
        reuse_base: true,
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
                Some(n) => g.baseline = n.clone(),
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
            "--no-reuse-base" => g.reuse_base = false,
            "--deps" => g.deps = true,
            "--allow-gone" => g.allow_gone = true,
            other => return err(&format!("unknown gate arg {other:?}")),
        }
    }

    if let Err(e) = gate_config::apply(&mut g.common) {
        return err(&e);
    }

    let mut failures = 0u32;
    let mut failing_ids: Vec<String> = Vec::new();

    let (reference, current, short_circuited) = match resolve(&g) {
        Ok(Some(triple)) => triple,
        // Newly measured crate: nothing at the ref to regress against.
        // Passing is the honest answer; failing would make a crate's
        // first bench permanently ungateable until it is already merged.
        Ok(None) => {
            println!(
                "gate: no bench target at {} for {} — newly measured, nothing to compare",
                g.against_ref.as_deref().unwrap_or("?"),
                g.common.pkg.as_deref().unwrap_or("this package")
            );
            return 0;
        }
        Err(e) => return err(&e),
    };
    // A gate that measured nothing must never pass: a typo'd --filter, an
    // empty registry, or lost records would otherwise be permanently green.
    if current.items.is_empty() {
        return err("measured 0 items — check --filter / registry setup; refusing to gate");
    }
    if let Some(b) = &current.gating_backend {
        println!("gate: gating backend = {b}");
    }
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
        pkg: g.common.pkg.clone(),
        buildcost_mode,
        accepted,
    };
    failures += compare(
        &reference,
        &current,
        "",
        &ctx,
        &mut failing_ids,
        &mut Vec::new(),
    );

    // Ratchet: also compare against a long-lived baseline (e.g. last release)
    // so ten +4% regressions can't compound past a single gate.
    if let Some(ratchet_name) = &g.ratchet {
        match invoke::load_baseline(ratchet_name) {
            Ok(Some(r)) => {
                println!("--- ratchet: {ratchet_name} ---");
                let rctx = CompareCtx {
                    deps_mode: false,
                    ..ctx.clone()
                };
                failures += compare(
                    &r,
                    &current,
                    "RATCHET ",
                    &rctx,
                    &mut failing_ids,
                    &mut Vec::new(),
                );
            }
            Ok(None) => return err(&format!("no ratchet baseline named {ratchet_name:?}")),
            Err(e) => return err(&format!("failed to load ratchet baseline: {e}")),
        }
    }

    // Checked claims from the runner.
    for a in &current.assertions {
        let verdict = if a.ok { "ok" } else { "FAIL" };
        if !a.ok {
            failures += 1;
            failing_ids.push(a.id.clone());
        }
        println!("{verdict:<5} {} assert {}: {}", a.id, a.kind, a.detail);
    }

    write_gate_status(failures);
    if let Some(name) = &g.save_baseline {
        match save_verdict(failures, short_circuited) {
            SaveVerdict::Save => {
                match invoke::save_baseline(name, &current, invoke::SaveScope::of(&g.common)) {
                    Ok(path) => println!("baseline saved: {}", path.display()),
                    Err(e) => {
                        eprintln!("soothfast: failed to save baseline: {e}");
                        return 1;
                    }
                }
            }
            SaveVerdict::RefuseFailing => {
                eprintln!("soothfast: refusing to save baseline {name:?}: gate failed");
            }
            SaveVerdict::SkipShortCircuit => println!(
                "gate: not saving baseline {name:?} — the short-circuit pass skipped the \
                 gating counters, and unchanged code leaves the previous baseline valid"
            ),
        }
    }
    if failures > 0 {
        failing_ids.sort();
        failing_ids.dedup();
        // buildcost pseudo-items have no runnable body to profile.
        failing_ids.retain(|id| !id.starts_with("buildcost::"));
        triage(&g.common, &failing_ids);
        println!("gate: FAILED ({failures} regression(s))");
        1
    } else {
        println!("gate: passed ({} item(s))", current.items.len());
        0
    }
}

/// Resolve the comparison pair for a gate run: reference doc, current run,
/// and whether the current run was a short-circuited identical-binaries
/// pass. `Ok(None)` only in `--against-ref` mode: a crate newly measured has
/// nothing at the ref to compare against.
fn resolve(g: &GateArgs) -> Result<Option<(Value, Run, bool)>, String> {
    if g.common.backend.as_deref() == Some("buildcost") {
        let Some(pkg) = g.common.pkg.clone() else {
            return Err("--backend buildcost requires -p PKG".into());
        };
        let current = crate::buildcost::measure(&pkg, &g.matrix)?;
        let reference = if let Some(refname) = &g.against_ref {
            // Merge-base build cost, measured in a worktree. Warm the dep
            // graph un-timed first: the worktree's target dir starts cold,
            // and dep compilation isn't this package's build cost.
            let base = invoke::with_merge_base_worktree(refname, |wt| {
                invoke::sync_untracked_cargo_config(wt)?;
                let warm = std::process::Command::new("cargo")
                    .args(["build", "--release", "-p", &pkg])
                    .current_dir(wt)
                    .status()
                    .map_err(|e| e.to_string())?;
                if !warm.success() {
                    return Err(format!("merge-base warmup build failed for {pkg}"));
                }
                crate::buildcost::measure_in(&pkg, &g.matrix, Some(wt))
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
        return Ok(Some((reference, current, false)));
    }
    if let Some(refname) = &g.against_ref {
        return match measure_ref_interleaved(&g.common, refname, g.reuse_base)? {
            Some(pair) => Ok(Some((pair.reference, pair.current, pair.short_circuited))),
            None => Ok(None),
        };
    }
    let records = invoke::run_bench(&g.common, &[]).map_err(|e| e.to_string())?;
    let mut current = invoke::collect(&records);
    current.build = Some(buildstamp::capture(g.common.codegen_units_env(), None));
    let baseline = invoke::load_baseline(&g.baseline)
        .map_err(|e| format!("failed to load baseline: {e}"))?
        .ok_or_else(|| {
            format!(
                "no baseline named {:?} — create one with `cargo soothfast measure --save-baseline {}`",
                g.baseline, g.baseline
            )
        })?;
    Ok(Some((baseline, current, false)))
}

struct AcceptArgs {
    common: CommonArgs,
    baseline: String,
    against_ref: Option<String>,
    matrix: String,
    justification: Option<String>,
    only: Vec<String>,
}

/// Record a reviewed, currently-failing delta into `soothfast-gate.lock`, so
/// a future `gate` treats it as the new ceiling for that bench and metric
/// instead of a regression — without touching the threshold for anything
/// else. Refuses a bench that isn't currently failing: an unearned entry
/// would let the lock file accumulate speculative headroom instead of a
/// reviewed record of what actually shifted.
fn accept_cmd(args: &[String]) -> i32 {
    let mut a = AcceptArgs {
        common: CommonArgs::default(),
        baseline: "base".into(),
        against_ref: None,
        matrix: "default".into(),
        justification: None,
        only: Vec::new(),
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
            other => return err(&format!("unknown gate accept arg {other:?}")),
        }
    }
    if let Err(e) = gate_config::apply(&mut a.common) {
        return err(&e);
    }
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
        matrix: a.matrix,
        save_baseline: None,
        reuse_base: true,
    };
    let (reference, current, _) = match resolve(&g) {
        Ok(Some(triple)) => triple,
        Ok(None) => {
            println!("gate accept: nothing to compare — nothing to accept");
            return 0;
        }
        Err(e) => return err(&e),
    };
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
        pkg: g.common.pkg.clone(),
        buildcost_mode: g.common.backend.as_deref() == Some("buildcost"),
        accepted: entries.clone(),
    };
    let mut failed_metrics = Vec::new();
    compare(
        &reference,
        &current,
        "",
        &ctx,
        &mut Vec::new(),
        &mut failed_metrics,
    );

    let failing_ids: BTreeSet<&str> = failed_metrics.iter().map(|f| f.id.as_str()).collect();
    for id in &a.only {
        if !failing_ids.contains(id.as_str()) {
            return err(&format!(
                "{id} is not currently failing — nothing to accept"
            ));
        }
    }
    if failed_metrics.is_empty() {
        println!("gate accept: nothing is failing — nothing to accept");
        return 0;
    }

    let mut by_id: BTreeMap<&str, Vec<&FailedMetric>> = BTreeMap::new();
    for m in &failed_metrics {
        if a.only.is_empty() || a.only.iter().any(|id| id == &m.id) {
            by_id.entry(m.id.as_str()).or_default().push(m);
        }
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

    if let Err(e) = gate_lock::write(&root, &entries) {
        return err(&format!("failed to write {}: {e}", gate_lock::LOCKFILE));
    }
    println!(
        "gate accept: recorded {} bench(es) in {}",
        by_id.len(),
        gate_lock::LOCKFILE
    );
    0
}

/// Whether a `--save-baseline` request may persist this run. A failing run
/// is never saved (it would ratify its own regression), and the identical-
/// binaries short-circuit run carries no gating counters — saving it would
/// hand downstream readers a baseline with the instruction counts missing.
#[derive(Debug, PartialEq)]
enum SaveVerdict {
    Save,
    RefuseFailing,
    SkipShortCircuit,
}

fn save_verdict(failures: u32, short_circuited: bool) -> SaveVerdict {
    if failures > 0 {
        SaveVerdict::RefuseFailing
    } else if short_circuited {
        SaveVerdict::SkipShortCircuit
    } else {
        SaveVerdict::Save
    }
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

/// A measured (reference, head) pair from the merge-base worktree.
struct RefPair {
    reference: Value,
    current: Run,
    /// The identical-binaries short circuit ran one timing-only pass, so
    /// `current` has no gating counters and is not baseline material.
    short_circuited: bool,
}

/// Measure the merge-base with HEAD in a temp worktree, interleaving rounds
/// (head, base, head, base) tango-style so slow environmental drift cancels;
/// per-metric minima across each side's rounds form the comparison values.
/// Interleaving is a timing concern: the deterministic gating counters
/// (perfcnt/callgrind) don't drift, so the second round of each side skips
/// them and re-measures only the timing-sensitive backends.
/// `Ok(None)` when the merge-base has no such bench target: the crate is
/// newly measured, so there is nothing on that side to compare against.
/// A merge-base already measured under the same conditions is served from
/// the run cache instead of being rebuilt, unless `reuse` is false.
fn measure_ref_interleaved(
    common: &CommonArgs,
    refname: &str,
    reuse: bool,
) -> Result<Option<RefPair>, String> {
    const TIMING_ONLY: &[&str] = &["--skip-gating-counters"];
    enum Ref {
        NoBenchTarget,
        IdenticalBinaries(Result<Run, String>),
        /// Reference served by binary identity rather than by commit.
        Cached(Value),
        Rounds(Box<[Result<Run, String>; 4]>),
    }

    // Worktree builds go to a persistent sibling of the parent target dir so
    // dependencies compile once per machine, not once per gate run.
    let wt_target = invoke::worktree_target_dir().ok();
    let measure = |dir: Option<&std::path::Path>, extra: &[&str]| -> Result<Run, String> {
        let td = dir.and(wt_target.as_deref());
        let recs = invoke::run_bench_dir(common, extra, dir, td).map_err(|e| e.to_string())?;
        let mut run = invoke::collect(&recs);
        run.build = Some(buildstamp::capture(common.codegen_units_env(), dir));
        Ok(run)
    };

    let base_sha = invoke::git(&["merge-base", "HEAD", refname])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let head_stamp = buildstamp::capture(common.codegen_units_env(), None);
    let cache_key =
        (!base_sha.is_empty() && reuse).then(|| runcache::key(&base_sha, &head_stamp, common));

    if let Some(k) = &cache_key
        && let Some(doc) = runcache::load(k, &base_sha)
    {
        println!("gate: reusing the measured merge-base {base_sha}");
        let head = match head_from_runcache(common, &head_stamp) {
            Some(head) => {
                println!("gate: reusing HEAD's own already-measured run");
                head
            }
            None => measure(None, &[])?,
        };
        cache_head(common, &head_stamp, &head);
        return Ok(Some(RefPair {
            reference: doc,
            current: head,
            short_circuited: false,
        }));
    }

    println!("gate: measuring merge-base of {refname} in worktree (interleaved rounds)");
    let pkg = common.pkg.clone();
    let target = common.target.clone().unwrap_or_else(|| "soothfast".into());
    let outcome = invoke::with_merge_base_worktree(refname, |wt| {
        // Only checkable when a package was named; without `-p` cargo picks
        // the default member and there is nothing to look up.
        if let Some(pkg) = &pkg
            && !workspace::has_bench_target(pkg, &target, Some(wt)).map_err(|e| e.to_string())?
        {
            return Ok((Ref::NoBenchTarget, None));
        }
        // A local .cargo/config.toml (untracked, e.g. a linker pin) wouldn't
        // otherwise reach the worktree and would read as a false mismatch.
        invoke::sync_untracked_cargo_config(wt)?;
        // Both bench binaries must embed the same soothfast harness: a
        // measurement-protocol change between the two locked versions would
        // otherwise be reported as a regression in the measured project.
        // Pinning must precede the base build the binary comparison does.
        invoke::sync_harness_versions(wt)?;
        let digests = bench_text_digests(common, wt, wt_target.as_deref());
        // A run measured from byte-identical machine code under the same
        // conditions is that binary's measurement, whatever commit built it.
        let by_binary = match (reuse, &digests) {
            (true, Some((_, base))) => {
                runcache::load(&runcache::key(base, &head_stamp, common), &base_sha)
            }
            _ => None,
        };

        if digests.as_ref().is_some_and(|(head, base)| head == base) {
            println!(
                "gate: bench binaries identical (.text match) — no measurable change possible"
            );
            // Head shares that machine code, so the cached run is its
            // measurement too, and the next commit can reuse it from HEAD.
            if let Some(doc) = &by_binary {
                cache_head_doc(common, &head_stamp, doc);
            }
            let args = identical_pass_args(common.backend.as_deref());
            return Ok((Ref::IdenticalBinaries(measure(None, args)), digests));
        }
        if let Some(doc) = by_binary {
            println!("gate: reusing a run measured from the same merge-base binary");
            return Ok((Ref::Cached(doc), digests));
        }
        Ok((
            Ref::Rounds(Box::new([
                measure(None, &[]),
                measure(Some(wt), &[]),
                measure(None, TIMING_ONLY),
                measure(Some(wt), TIMING_ONLY),
            ])),
            digests,
        ))
    })?;
    let (outcome, digests) = outcome;

    let (base, head) = match outcome {
        Ref::NoBenchTarget => return Ok(None),
        // One cheap pass serves as both sides: real items and assertions,
        // zero deltas by construction.
        Ref::IdenticalBinaries(run) => {
            let head = run?;
            return Ok(Some(RefPair {
                reference: ref_doc(&head),
                current: head,
                short_circuited: true,
            }));
        }
        Ref::Cached(doc) => {
            let head = match head_from_runcache(common, &head_stamp) {
                Some(head) => head,
                None => measure(None, &[])?,
            };
            cache_head(common, &head_stamp, &head);
            return Ok(Some(RefPair {
                reference: doc,
                current: head,
                short_circuited: false,
            }));
        }
        Ref::Rounds(rounds) => {
            let [head1, base1, head2, base2] = *rounds;
            (combine_rounds(base1?, base2), combine_rounds(head1?, head2))
        }
    };
    let reference = ref_doc(&base);
    if let Some(k) = &cache_key {
        runcache::store(k, &reference);
    }
    // Keyed by machine code as well as by commit, so a later merge-base
    // that builds the same binary hits even if it was never gated.
    if reuse && let Some((head_digest, base_digest)) = &digests {
        runcache::store(&runcache::key(base_digest, &head_stamp, common), &reference);
        runcache::store(
            &runcache::key(head_digest, &head_stamp, common),
            &ref_doc(&head),
        );
    }
    cache_head(common, &head_stamp, &head);
    Ok(Some(RefPair {
        reference,
        current: head,
        short_circuited: false,
    }))
}

/// HEAD's own measurement, if a clean-tree run already cached one under this
/// commit and these conditions — e.g. the `gate` run `gate accept` follows
/// moments later. Avoids paying for a second full measurement of the same
/// commit just to build the accept report.
fn head_from_runcache(common: &CommonArgs, stamp: &buildstamp::BuildStamp) -> Option<Run> {
    if !invoke::tree_is_clean() {
        return None;
    }
    let sha = invoke::git(&["rev-parse", "HEAD"]).ok()?;
    let sha = sha.trim();
    let doc = runcache::load(&runcache::key(sha, stamp, common), sha)?;
    Some(invoke::run_from_items_value(&doc["items"]))
}

/// Keep HEAD's run so the next commit's gate finds its reference already
/// measured. Only from a clean tree, where HEAD's SHA names what was built.
fn cache_head(common: &CommonArgs, stamp: &buildstamp::BuildStamp, head: &Run) {
    cache_head_doc(common, stamp, &ref_doc(head));
}

/// Keep `doc` as HEAD's measurement.
fn cache_head_doc(common: &CommonArgs, stamp: &buildstamp::BuildStamp, doc: &Value) {
    if !invoke::tree_is_clean() {
        return;
    }
    let Ok(sha) = invoke::git(&["rev-parse", "HEAD"]) else {
        return;
    };
    runcache::store(&runcache::key(sha.trim(), stamp, common), doc);
}

/// Runner args for the single pass taken when both sides' binaries match.
/// A backend named on the command line may be one of the gating counters,
/// which measures nothing once those counters are skipped, so it keeps them.
fn identical_pass_args(backend: Option<&str>) -> &'static [&'static str] {
    match backend {
        Some("perfcnt" | "callgrind") => &[],
        _ => &["--skip-gating-counters"],
    }
}

/// A measured run in the reference-document shape `compare` reads.
fn ref_doc(run: &Run) -> Value {
    let mut doc = serde_json::json!({ "version": 1 });
    if let Some(n) = run.noise_pct {
        doc["noise_pct"] = serde_json::json!(n);
    }
    if let Some(b) = &run.build {
        doc["build"] = b.to_json();
    }
    doc["items"] = invoke::run_to_items_value(run);
    doc
}

/// Digests of the head and merge-base bench binaries' machine code.
/// Conservative: any build or extraction failure yields `None`, so the gate
/// falls through to a real measurement.
fn bench_text_digests(
    common: &CommonArgs,
    wt: &std::path::Path,
    wt_target: Option<&std::path::Path>,
) -> Option<(String, String)> {
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
    let digest = |p: &std::path::Path| {
        text_section_bytes(p).map(|b| format!("{:016x}", soothfast_registry::fnv1a(&b)))
    };
    Some((digest(&head)?, digest(&base)?))
}

/// The executable's `.text` section, extracted with objcopy. Whole-file
/// comparison would never match: debug info embeds the absolute build path,
/// and the two sides build in different directories.
fn text_section_bytes(exe: &std::path::Path) -> Option<Vec<u8>> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let out = std::env::temp_dir().join(format!(
        "soothfast-text-{}-{}.bin",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
    ));
    let status = std::process::Command::new("objcopy")
        .args(["-O", "binary", "--only-section=.text"])
        .arg(exe)
        .arg(&out)
        .status();
    let bytes = match status {
        Ok(s) if s.success() => std::fs::read(&out).ok(),
        _ => None,
    };
    let _ = std::fs::remove_file(&out);
    bytes.filter(|b| !b.is_empty())
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
    /// Package being gated (`-p`, raw name); baseline entries from other
    /// packages are out of scope, not GONE.
    pkg: Option<String>,
    buildcost_mode: bool,
    accepted: gate_lock::Entries,
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
/// every metric that still fails after accepted overrides are applied.
fn compare(
    reference: &Value,
    current: &Run,
    label: &str,
    ctx: &CompareCtx,
    failing_ids: &mut Vec<String>,
    out_failed: &mut Vec<FailedMetric>,
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

        // Hard-gated relative metrics. A `downgrade` reason turns a hard
        // fail into a SOFT warning (walltime anti-flake rules). An accepted
        // entry — reviewed, one-sided ceiling anchored at its own value —
        // takes precedence over a plain FAIL, but not over a downgrade.
        let mut rel = |metric: &str,
                       old_v: Option<f64>,
                       new_v: Option<f64>,
                       limit: f64,
                       hard: bool,
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
                (true, false, _, _) => ("SOFT", None),
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
            build_soft,
        );
        rel(
            "callgrind_ir",
            old["callgrind"]["ir"].as_u64().map(|v| v as f64),
            cur.ir.map(|v| v as f64),
            counter_limit(IR_THRESHOLD_PCT, cur),
            true,
            build_soft,
        );
        rel(
            "walltime_median_ns",
            old["walltime"]["median_ns"].as_f64(),
            cur.median_ns,
            counter_limit(wall_limit, cur),
            true,
            walltime_downgrade(old, cur, wall_limit)
                .or(build_soft)
                .or(reused_soft),
        );
        rel(
            "build_ms",
            old["buildcost"]["build_ms"].as_u64().map(|v| v as f64),
            cur.build_ms.map(|v| v as f64),
            BUILD_MS_SOFT_PCT,
            false,
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
            let filtered_out = ctx.filter.as_deref().is_some_and(|f| !id.contains(f));
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

#[cfg(test)]
mod tests {
    use super::{
        CompareCtx, FailedMetric, SaveVerdict, combine_min, combine_rounds, compare,
        identical_pass_args, save_verdict, text_section_bytes, walltime_limit,
    };
    use crate::buildstamp;
    use crate::gate_lock;
    use crate::invoke::{ItemMetrics, Run};
    use serde_json::{Value, json};

    fn ctx() -> CompareCtx {
        CompareCtx {
            deps_mode: false,
            allow_gone: false,
            filter: None,
            pkg: None,
            buildcost_mode: false,
            accepted: gate_lock::Entries::new(),
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
        let failures = compare(reference, current, "", &ctx, &mut failing, &mut Vec::new());
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

    #[test]
    fn out_failed_collects_failing_metrics_for_accept() {
        let reference = drift_reference(Some(stamp("aaaa").to_json()));
        let head = drift_head(Some(stamp("aaaa")), None);
        let mut failing = Vec::new();
        let mut out: Vec<FailedMetric> = Vec::new();
        compare(&reference, &head, "", &ctx(), &mut failing, &mut out);
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
        compare(&reference, &head, "", &ctx, &mut failing, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn a_named_gating_backend_keeps_its_counters_on_the_identical_pass() {
        assert!(identical_pass_args(Some("perfcnt")).is_empty());
        assert!(identical_pass_args(Some("callgrind")).is_empty());
    }

    #[test]
    fn the_identical_pass_skips_counters_otherwise() {
        assert_eq!(identical_pass_args(None), ["--skip-gating-counters"]);
        assert_eq!(
            identical_pass_args(Some("walltime")),
            ["--skip-gating-counters"]
        );
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

    #[test]
    fn a_passing_full_run_is_baseline_material() {
        assert_eq!(save_verdict(0, false), SaveVerdict::Save);
    }

    #[test]
    fn a_failing_run_is_never_saved() {
        // Saving it would ratify its own regression.
        assert_eq!(save_verdict(1, false), SaveVerdict::RefuseFailing);
        assert_eq!(save_verdict(3, true), SaveVerdict::RefuseFailing);
    }

    #[test]
    fn a_short_circuit_run_lacks_counters_and_is_skipped() {
        assert_eq!(save_verdict(0, true), SaveVerdict::SkipShortCircuit);
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

    #[test]
    fn text_extraction_returns_none_for_a_non_elf_file() {
        let path = std::env::temp_dir().join(format!("soothfast-not-elf-{}", std::process::id()));
        std::fs::write(&path, b"just text, no sections").unwrap();
        assert_eq!(text_section_bytes(&path), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn text_extraction_returns_none_for_a_missing_file() {
        assert_eq!(
            text_section_bytes(std::path::Path::new("/nonexistent/soothfast-bench")),
            None
        );
    }

    #[test]
    fn an_executable_text_section_matches_itself() {
        let objcopy_present = std::process::Command::new("objcopy")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !objcopy_present {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let a = text_section_bytes(&exe).expect("test binary has a .text section");
        assert_eq!(Some(a), text_section_bytes(&exe));
    }
}
