//! The regression gate: re-measure, compare against a stored baseline (or a
//! merge-base worktree with `--against-ref`) with per-metric thresholds,
//! exit non-zero on regression. On failure, emits a callgrind triage
//! artifact when valgrind is available.

use serde_json::Value;

use crate::invoke::{self, CommonArgs, Run};
use crate::workspace;

/// Deterministic counters gate tight.
pub const INSTRUCTIONS_THRESHOLD_PCT: f64 = 5.0;
const IR_THRESHOLD_PCT: f64 = 5.0;
/// Walltime medians gate at +10% — but only when the observed A/A noise
/// floor is safely inside that margin (refuse to gate on noise).
const WALLTIME_THRESHOLD_PCT: f64 = 10.0;
/// Allocation counts/bytes and binary size are deterministic; gate at +5%.
pub const ALLOC_THRESHOLD_PCT: u64 = 5;
const SIZE_THRESHOLD_PCT: u64 = 5;
/// Poll/wake counts on the counting executor are exact; gate them like
/// allocations. This is the regression class instruction counts miss — a
/// future that polls twice as often can have an identical `Ir`.
const ASYNC_THRESHOLD_PCT: u64 = 5;
/// Compile time is noisy: soft (warn-only) at +25%.
const BUILD_MS_SOFT_PCT: f64 = 25.0;

struct GateArgs {
    common: CommonArgs,
    baseline: String,
    ratchet: Option<String>,
    against_ref: Option<String>,
    deps: bool,
    allow_gone: bool,
    matrix: String,
}

pub fn run(args: &[String]) -> i32 {
    let mut g = GateArgs {
        common: CommonArgs::default(),
        baseline: "base".into(),
        ratchet: None,
        against_ref: None,
        deps: false,
        allow_gone: false,
        matrix: "default".into(),
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
            "--deps" => g.deps = true,
            "--allow-gone" => g.allow_gone = true,
            other => return err(&format!("unknown gate arg {other:?}")),
        }
    }

    let mut failures = 0u32;
    let mut failing_ids: Vec<String> = Vec::new();

    // Reference + current: merge-base worktree (interleaved) or stored baseline.
    let (reference, current) = if g.common.backend.as_deref() == Some("buildcost") {
        let Some(pkg) = g.common.pkg.clone() else {
            return err("--backend buildcost requires -p PKG");
        };
        let current = match crate::buildcost::measure(&pkg, &g.matrix) {
            Ok(r) => r,
            Err(e) => return err(&e),
        };
        let reference = if let Some(refname) = &g.against_ref {
            // Merge-base build cost, measured in a worktree. Warm the dep
            // graph un-timed first: the worktree's target dir starts cold,
            // and dep compilation isn't this package's build cost.
            let base = invoke::with_merge_base_worktree(refname, |wt| {
                let warm = std::process::Command::new("cargo")
                    .args(["build", "--release", "-p", &pkg])
                    .current_dir(wt)
                    .status()
                    .map_err(|e| e.to_string())?;
                if !warm.success() {
                    return Err(format!("merge-base warmup build failed for {pkg}"));
                }
                crate::buildcost::measure_in(&pkg, &g.matrix, Some(wt))
            });
            let base = match base {
                Ok(r) => r,
                Err(e) => return err(&e),
            };
            let mut doc = serde_json::json!({ "version": 1 });
            doc["items"] = invoke::run_to_items_value(&base);
            doc
        } else {
            let mut baseline = match invoke::load_baseline(&g.baseline) {
                Ok(Some(b)) => b,
                Ok(None) => return err(&format!("no baseline named {:?}", g.baseline)),
                Err(e) => return err(&format!("failed to load baseline: {e}")),
            };
            // Only buildcost items are in play; drop bench entries from the view.
            if let Some(map) = baseline["items"].as_object_mut() {
                map.retain(|k, _| k.starts_with("buildcost::"));
            }
            baseline
        };
        (reference, current)
    } else if let Some(refname) = &g.against_ref {
        match measure_ref_interleaved(&g.common, refname) {
            Ok(Some(pair)) => pair,
            // Newly measured crate: nothing at the ref to regress against.
            // Passing is the honest answer; failing would make a crate's
            // first bench permanently ungateable until it is already merged.
            Ok(None) => {
                println!(
                    "gate: no bench target at {refname} for {} — newly measured, nothing to compare",
                    g.common.pkg.as_deref().unwrap_or("this package")
                );
                return 0;
            }
            Err(e) => return err(&e),
        }
    } else {
        let records = match invoke::run_bench(&g.common, &[]) {
            Ok(r) => r,
            Err(e) => return err(&e.to_string()),
        };
        let current = invoke::collect(&records);
        let baseline = match invoke::load_baseline(&g.baseline) {
            Ok(Some(b)) => b,
            Ok(None) => {
                eprintln!(
                    "soothfast: no baseline named {:?} — create one with \
                     `cargo soothfast measure --save-baseline {}`",
                    g.baseline, g.baseline
                );
                return 2;
            }
            Err(e) => return err(&format!("failed to load baseline: {e}")),
        };
        (baseline, current)
    };
    // A gate that measured nothing must never pass: a typo'd --filter, an
    // empty registry, or lost records would otherwise be permanently green.
    if current.items.is_empty() {
        return err("measured 0 items — check --filter / registry setup; refusing to gate");
    }
    if let Some(b) = &current.gating_backend {
        println!("gate: gating backend = {b}");
    }
    let buildcost_mode = g.common.backend.as_deref() == Some("buildcost");
    let ctx = CompareCtx {
        deps_mode: g.deps,
        allow_gone: g.allow_gone,
        filter: g.common.filter.clone(),
        pkg: g.common.pkg.clone(),
        buildcost_mode,
    };
    failures += compare(&reference, &current, "", &ctx, &mut failing_ids);

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
                failures += compare(&r, &current, "RATCHET ", &rctx, &mut failing_ids);
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

/// Measure the merge-base with HEAD in a temp worktree, interleaving rounds
/// (head, base, head, base) tango-style so slow environmental drift cancels;
/// per-metric minima across each side's rounds form the comparison values.
/// `Ok(None)` when the merge-base has no such bench target: the crate is
/// newly measured, so there is nothing on that side to compare against.
fn measure_ref_interleaved(
    common: &CommonArgs,
    refname: &str,
) -> Result<Option<(Value, Run)>, String> {
    println!("gate: measuring merge-base of {refname} in worktree (interleaved rounds)");
    let measure = |dir: Option<&std::path::Path>| -> Result<Run, String> {
        let recs = invoke::run_bench_in(common, &[], dir).map_err(|e| e.to_string())?;
        Ok(invoke::collect(&recs))
    };
    let pkg = common.pkg.clone();
    let target = common.target.clone().unwrap_or_else(|| "soothfast".into());
    let Some((head1, base1, head2, base2)) = invoke::with_merge_base_worktree(refname, |wt| {
        // Only checkable when a package was named; without `-p` cargo picks
        // the default member and there is nothing to look up.
        if let Some(pkg) = &pkg
            && !workspace::has_bench_target(pkg, &target, Some(wt)).map_err(|e| e.to_string())?
        {
            return Ok(None);
        }
        Ok(Some((
            measure(None),
            measure(Some(wt)),
            measure(None),
            measure(Some(wt)),
        )))
    })?
    else {
        return Ok(None);
    };

    let base = combine_min(base1?, base2?);
    let head = combine_min(head1?, head2?);

    let mut doc = serde_json::json!({ "version": 1 });
    if let Some(n) = base.noise_pct {
        doc["noise_pct"] = serde_json::json!(n);
    }
    doc["items"] = invoke::run_to_items_value(&base);
    Ok(Some((doc, head)))
}

/// Per-metric minima of two rounds of the same side.
fn combine_min(mut first: Run, second: Run) -> Run {
    for (id, m2) in second.items {
        let m = first.items.entry(id).or_default();
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
}

/// Compare one reference document against the current run; returns failures
/// and records regressed item IDs for triage.
fn compare(
    reference: &Value,
    current: &Run,
    label: &str,
    ctx: &CompareCtx,
    failing_ids: &mut Vec<String>,
) -> u32 {
    let noise = current
        .noise_pct
        .unwrap_or(0.0)
        .max(reference["noise_pct"].as_f64().unwrap_or(0.0));
    let wall_gating = noise < WALLTIME_THRESHOLD_PCT / 2.0;
    println!(
        "{label}gate: noise_floor={noise:.2}% thresholds: instructions +{INSTRUCTIONS_THRESHOLD_PCT}% ir +{IR_THRESHOLD_PCT}% walltime +{WALLTIME_THRESHOLD_PCT}% alloc/size +{ALLOC_THRESHOLD_PCT}% polls/wakes +{ASYNC_THRESHOLD_PCT}%"
    );
    if !wall_gating {
        println!(
            "WARN: noise floor {noise:.2}% too close to +{WALLTIME_THRESHOLD_PCT}% — walltime not gated (deterministic metrics still are)"
        );
    }

    let old_items = &reference["items"];
    let mut failures = 0u32;
    let mut fingerprints_changed = false;
    let mut failed_here: Vec<String> = Vec::new();

    for (id, cur) in &current.items {
        let old = &old_items[id.as_str()];
        if old.is_null() {
            println!("NEW   {id} (no reference entry)");
            continue;
        }
        if let Some(fp) = old["fingerprint"].as_str()
            && !fp.is_empty()
            && fp != cur.fingerprint
        {
            fingerprints_changed = true;
            println!("NOTE  {id}: code changed since reference");
        }

        // Hard-gated relative metrics.
        let mut rel =
            |metric: &str, old_v: Option<f64>, new_v: Option<f64>, limit: f64, hard: bool| {
                let (Some(o), Some(n)) = (old_v, new_v) else {
                    return;
                };
                if o <= 0.0 {
                    return;
                }
                let delta = (n - o) / o * 100.0;
                let fail = delta > limit;
                let verdict = if fail && hard {
                    "FAIL"
                } else if fail {
                    "SOFT"
                } else {
                    "ok"
                };
                if fail && hard {
                    failures += 1;
                    failed_here.push(id.clone());
                }
                println!("{verdict:<5} {label}{id} {metric} {o:.1} -> {n:.1} ({delta:+.1}%)");
            };
        rel(
            "instructions",
            old["perfcnt"]["instructions"].as_u64().map(|v| v as f64),
            cur.instructions.map(|v| v as f64),
            INSTRUCTIONS_THRESHOLD_PCT,
            true,
        );
        rel(
            "callgrind_ir",
            old["callgrind"]["ir"].as_u64().map(|v| v as f64),
            cur.ir.map(|v| v as f64),
            IR_THRESHOLD_PCT,
            true,
        );
        if wall_gating {
            rel(
                "walltime_median_ns",
                old["walltime"]["median_ns"].as_f64(),
                cur.median_ns,
                WALLTIME_THRESHOLD_PCT,
                true,
            );
        }
        rel(
            "build_ms",
            old["buildcost"]["build_ms"].as_u64().map(|v| v as f64),
            cur.build_ms.map(|v| v as f64),
            BUILD_MS_SOFT_PCT,
            false,
        );

        // Integer metrics: old=0 allows nothing (zero-alloc stays zero-alloc).
        let mut int = |metric: &str, old_v: Option<u64>, new_v: Option<u64>, pct: u64| {
            let (Some(o), Some(n)) = (old_v, new_v) else {
                return;
            };
            let allowed = o + o * pct / 100;
            let fail = n > allowed;
            if fail {
                failures += 1;
                failed_here.push(id.clone());
            }
            let verdict = if fail { "FAIL" } else { "ok" };
            println!("{verdict:<5} {label}{id} {metric} {o} -> {n} (allowed <= {allowed})");
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
    failures
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
