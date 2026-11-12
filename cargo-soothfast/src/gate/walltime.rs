//! When a walltime failure is downgraded because the counters show nothing changed.

use serde_json::Value;

use super::thresholds::{COUNTER_FLAT_ABS, COUNTER_FLAT_PCT};
use crate::gate_lock;
use crate::invoke::ItemMetrics;

/// Why a walltime overrun is a SOFT warning instead of a hard fail, if it is.
pub(super) fn walltime_downgrade(
    old: &Value,
    cur: &ItemMetrics,
    limit: f64,
) -> Option<&'static str> {
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
pub(super) fn accepted_counters_walltime_downgrade(
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
