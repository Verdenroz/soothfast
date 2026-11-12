//! Metric-by-metric comparison of a run against its reference.

use serde_json::Value;

use super::thresholds::{
    ALLOC_THRESHOLD_PCT, ASYNC_THRESHOLD_PCT, BUILD_MS_SOFT_PCT, HEADROOM_COUNTER_FLOOR_PCT,
    INSTRUCTIONS_THRESHOLD_PCT, IR_THRESHOLD_PCT, SIZE_THRESHOLD_PCT, WALLTIME_NOISE_MARGIN,
    WALLTIME_THRESHOLD_PCT, counter_limit, imported_noise_pct, walltime_limit,
};
use super::walltime::{accepted_counters_walltime_downgrade, walltime_downgrade};
use crate::buildstamp;
use crate::gate_lock;
use crate::invoke::{self, Run};

/// How to recover when a regression was measured against a stored perfcnt
/// reference, which one bad reading could have skewed. Callgrind is exact,
/// so a reused callgrind reference needs no such doubt.
pub(super) fn reuse_hint(
    reference: &Value,
    backend: Option<&str>,
    regressions: u32,
) -> Option<String> {
    let from = reference["reused_from"].as_str()?;
    (regressions > 0 && backend == Some("perfcnt")).then(|| {
        format!("gate: reference reused from {from}; rerun with --no-reuse-base to re-measure")
    })
}

/// How one comparison run should treat scope and disappearance.
#[derive(Clone)]
pub(super) struct CompareCtx {
    pub(super) deps_mode: bool,
    pub(super) allow_gone: bool,
    pub(super) filter: Option<String>,
    /// Items other shards own are out of scope, not GONE.
    pub(super) shard: Option<invoke::Shard>,
    /// Package being gated (`-p`, raw name); baseline entries from other
    /// packages are out of scope, not GONE.
    pub(super) pkg: Option<String>,
    pub(super) buildcost_mode: bool,
    pub(super) accepted: gate_lock::Entries,
    /// `gate accept --headroom PCT`: also collect near-miss metrics within
    /// `PCT` points of failing. `None` for every ordinary `gate` run.
    pub(super) headroom_pct: Option<f64>,
    /// `[gate] walltime = "advisory"`: walltime overruns are SOFT.
    pub(super) walltime_advisory: bool,
}

/// A metric that failed its check, collected only when the caller (`gate
/// accept`) needs to build a new lock entry from it.
pub(super) struct FailedMetric {
    pub(super) id: String,
    pub(super) metric: String,
    pub(super) new_value: f64,
}

/// Compare one reference document against the current run; returns failures,
/// records regressed item IDs for triage, and (for `gate accept`) collects
/// every metric that still fails after accepted overrides are applied, plus
/// (with `ctx.headroom_pct` set) every passing metric close enough to
/// failing to be worth recording in the same accept.
pub(super) fn compare(
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

#[cfg(test)]
mod tests;
