//! Whether repeated perfcnt readings of one counter agree well enough to be
//! stored. A stored run is reused by every later gate with the same key, so a
//! single bad reading would otherwise fail them all.

/// How far apart two readings of one deterministic counter may be and still
/// count as the same measurement.
#[derive(Debug, Clone, Copy)]
pub struct Tolerance {
    pub pct: f64,
    /// Floor for small counts, where the percentage is tighter than the
    /// counter's own run-to-run wobble.
    pub abs: u64,
}

/// What to persist from repeated readings of one counter.
#[derive(Debug, PartialEq)]
pub enum Agreement {
    Store(u64),
    /// No two readings agreed: store nothing.
    Refuse {
        low: u64,
        high: u64,
    },
}

pub fn within(a: u64, b: u64, tol: Tolerance) -> bool {
    let diff = a.abs_diff(b);
    diff <= tol.abs || diff as f64 / a.min(b).max(1) as f64 * 100.0 <= tol.pct
}

/// Two readings that agree store the first. Otherwise a third is taken, and
/// the median of the three is stored when any two of them agree.
pub fn settle(first: u64, second: u64, third: impl FnOnce() -> u64, tol: Tolerance) -> Agreement {
    if within(first, second, tol) {
        return Agreement::Store(first);
    }
    let third = third();
    let mut all = [first, second, third];
    all.sort_unstable();
    if within(first, third, tol) || within(second, third, tol) {
        Agreement::Store(all[1])
    } else {
        Agreement::Refuse {
            low: all[0],
            high: all[2],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOL: Tolerance = Tolerance { pct: 0.5, abs: 150 };

    #[test]
    fn the_bad_reading_from_a_real_gate_does_not_agree() {
        assert!(!within(3_271_181, 3_504_110, TOL));
    }

    #[test]
    fn agreeing_readings_store_the_first_without_a_third() {
        let got = settle(
            3_504_108,
            3_504_111,
            || panic!("no third reading needed"),
            TOL,
        );
        assert_eq!(got, Agreement::Store(3_504_108));
    }

    #[test]
    fn a_third_reading_outvotes_the_bad_one() {
        assert_eq!(
            settle(3_271_181, 3_504_110, || 3_504_111, TOL),
            Agreement::Store(3_504_110)
        );
    }

    #[test]
    fn three_disagreeing_readings_store_nothing() {
        assert_eq!(
            settle(3_271_181, 3_504_110, || 3_389_000, TOL),
            Agreement::Refuse {
                low: 3_271_181,
                high: 3_504_110
            }
        );
    }

    #[test]
    fn small_counts_agree_within_the_absolute_floor() {
        assert!(within(1_000, 1_140, TOL));
        assert!(!within(1_000, 1_200, TOL));
    }

    #[test]
    fn large_counts_agree_within_the_percentage() {
        assert!(within(1_000_000, 1_004_000, TOL));
        assert!(!within(1_000_000, 1_006_000, TOL));
    }
}
