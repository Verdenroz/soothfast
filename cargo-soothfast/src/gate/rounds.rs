//! Two-round measurement: duration formatting and merging a second round into the first.

use std::time::Duration;

use crate::invoke::Run;

pub(super) fn round_duration(d: Duration) -> String {
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
pub(super) fn combine_rounds(first: Run, second: Result<Run, String>) -> Run {
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

#[cfg(test)]
mod tests {
    use super::{combine_min, combine_rounds, round_duration};
    use crate::invoke::{ItemMetrics, Run};

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
}
