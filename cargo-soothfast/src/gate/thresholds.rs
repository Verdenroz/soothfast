//! Per-metric regression thresholds and the limits derived from them.

use serde_json::Value;

use crate::invoke::ItemMetrics;

/// Deterministic counters gate tight.
pub const INSTRUCTIONS_THRESHOLD_PCT: f64 = 5.0;
pub(super) const IR_THRESHOLD_PCT: f64 = 5.0;
/// Walltime medians gate at +10%, or higher on noisy runners (see
/// `walltime_limit`).
pub(super) const WALLTIME_THRESHOLD_PCT: f64 = 10.0;
/// Multiple of the A/A noise floor the walltime threshold must stay above.
/// 3x keeps the threshold at ≥3 sigma, where a suite of a few hundred items
/// expects well under one false positive per run.
pub(super) const WALLTIME_NOISE_MARGIN: f64 = 3.0;
/// Allocation counts/bytes and binary size are deterministic; gate at +5%.
pub const ALLOC_THRESHOLD_PCT: u64 = 5;
pub(super) const SIZE_THRESHOLD_PCT: u64 = 5;
/// Poll/wake counts on the counting executor are exact; gate them like
/// allocations. This is the regression class instruction counts miss — a
/// future that polls twice as often can have an identical `Ir`.
pub(super) const ASYNC_THRESHOLD_PCT: u64 = 5;
/// Compile time is noisy: soft (warn-only) at +25%.
pub(super) const BUILD_MS_SOFT_PCT: f64 = 25.0;
/// A deterministic counter within this of the reference reads as "flat" —
/// wide enough for perfcnt's tiny run-to-run wobble, far below any real change.
pub(crate) const COUNTER_FLAT_PCT: f64 = 0.5;
/// Absolute floor alongside `COUNTER_FLAT_PCT`: below ~30K instructions the
/// 0.5% cutoff is tighter than perfcnt's own run-to-run wobble.
pub(crate) const COUNTER_FLAT_ABS: u64 = 150;
/// `gate accept --headroom` floor for deterministic counters: a delta must
/// clear this before it counts as "close to failing," so a bench parked
/// near the line by measurement noise alone never qualifies.
pub(super) const HEADROOM_COUNTER_FLOOR_PCT: f64 = 3.0 * COUNTER_FLAT_PCT;

/// Effective walltime threshold: the fixed +10%, raised to 3x the measured
/// A/A noise floor when that is higher. A fixed threshold is ~2 sigma on a
/// noisy runner — several false positives per run across a few hundred
/// items — while dropping walltime entirely would leave such runners
/// unmonitored. Scaling keeps every run gated at ≥3 sigma.
pub(super) fn walltime_limit(noise_pct: f64) -> f64 {
    WALLTIME_THRESHOLD_PCT.max(noise_pct * WALLTIME_NOISE_MARGIN)
}

/// Relative walltime dispersion of an imported item, the larger of its two
/// sides, in percent. A harness result carries no A/A run, so its own spread
/// stands in for the noise floor. Zero for a natively measured item.
pub(super) fn imported_noise_pct(old: &Value, cur: &ItemMetrics) -> f64 {
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
pub(super) fn counter_limit(default_pct: f64, cur: &ItemMetrics) -> f64 {
    default_pct.max(cur.tolerance_pct.unwrap_or(0.0))
}

#[cfg(test)]
mod tests {
    use super::walltime_limit;

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
}
