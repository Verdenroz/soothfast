//! Complexity-claim evaluation over a size sweep.
//!
//! Gate philosophy: reject only on high-confidence
//! violation — the claim must not be *slower* than measured growth allows.
//! Adjacent classes (n vs n log n) are deliberately tolerated; a linear
//! claim on quadratic code is caught decisively.

/// Cost model for a claimed class at size n.
pub fn class_value(class: &str, n: f64) -> f64 {
    match class {
        "1" => 1.0,
        "log n" => n.ln().max(1.0),
        "n" => n,
        "n log n" => n * n.ln().max(1.0),
        "n^2" => n * n,
        _ => f64::NAN,
    }
}

/// Growth beyond the claim must stay under this factor across the sweep.
/// 2.5x tolerates n vs n log n ambiguity (~1.4x over a 16x size range)
/// while catching n -> n^2 (16x) the day it happens.
pub const DRIFT_LIMIT: f64 = 2.5;

/// Verdict of one complexity sweep: how far measured growth strayed from
/// the claimed class, and whether it stayed within [`DRIFT_LIMIT`].
#[derive(Debug, Clone, Copy)]
pub struct SweepOutcome {
    /// Measured growth divided by claimed growth, first size -> last size.
    pub drift: f64,
    /// True when `drift` is within [`DRIFT_LIMIT`] of the claimed class.
    pub ok: bool,
}

/// Evaluate measured values (one per size, same order) against a claim.
pub fn evaluate(class: &str, sizes: &[usize], values: &[f64]) -> SweepOutcome {
    assert_eq!(sizes.len(), values.len());
    assert!(sizes.len() >= 2, "sweep needs at least 2 sizes");
    let norm = |i: usize| values[i] / class_value(class, sizes[i] as f64);
    let first = norm(0);
    let last = norm(sizes.len() - 1);
    let drift = if first > 0.0 {
        last / first
    } else {
        f64::INFINITY
    };
    SweepOutcome {
        drift,
        ok: drift <= DRIFT_LIMIT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZES: &[usize] = &[1024, 4096, 16384];

    #[test]
    fn linear_claim_on_linear_code_passes() {
        let values: Vec<f64> = SIZES.iter().map(|&n| n as f64 * 3.0).collect();
        assert!(evaluate("n", SIZES, &values).ok);
    }

    #[test]
    fn linear_claim_on_quadratic_code_fails() {
        let values: Vec<f64> = SIZES.iter().map(|&n| (n * n) as f64).collect();
        let out = evaluate("n", SIZES, &values);
        assert!(!out.ok);
        assert!(out.drift > 10.0);
    }

    #[test]
    fn nlogn_claim_on_nlogn_code_passes() {
        let values: Vec<f64> = SIZES
            .iter()
            .map(|&n| n as f64 * (n as f64).ln() * 1.7)
            .collect();
        assert!(evaluate("n log n", SIZES, &values).ok);
    }

    #[test]
    fn adjacent_class_ambiguity_is_tolerated() {
        // n log n code under an "n" claim: drift = ln(16384)/ln(1024) ≈ 1.4.
        let values: Vec<f64> = SIZES.iter().map(|&n| n as f64 * (n as f64).ln()).collect();
        assert!(evaluate("n", SIZES, &values).ok);
    }

    #[test]
    fn faster_than_claimed_passes() {
        // Linear code under an "n^2" claim: drift < 1 — never a perf bug.
        let values: Vec<f64> = SIZES.iter().map(|&n| n as f64).collect();
        assert!(evaluate("n^2", SIZES, &values).ok);
    }
}
