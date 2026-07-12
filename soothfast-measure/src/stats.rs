//! Hand-rolled summary statistics (dependency policy: no stats crates).

/// Robust summary of one sample set.
#[derive(Debug, Clone, Copy)]
pub struct Summary {
    pub median: f64,
    /// Median absolute deviation, unscaled (no 1.4826 consistency constant).
    pub mad: f64,
    pub min: f64,
    pub max: f64,
}

/// Summarize a non-empty sample set. Sorts in place.
pub fn summarize(values: &mut [f64]) -> Summary {
    assert!(!values.is_empty(), "cannot summarize an empty sample set");
    values.sort_by(|a, b| a.partial_cmp(b).expect("NaN in samples"));
    let median = median_of_sorted(values);
    let mut deviations: Vec<f64> = values.iter().map(|v| (v - median).abs()).collect();
    deviations.sort_by(|a, b| a.partial_cmp(b).expect("NaN in deviations"));
    Summary {
        median,
        mad: median_of_sorted(&deviations),
        min: values[0],
        max: values[values.len() - 1],
    }
}

fn median_of_sorted(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_count() {
        let mut v = vec![5.0, 1.0, 3.0];
        let s = summarize(&mut v);
        assert_eq!(s.median, 3.0);
        assert_eq!(s.min, 1.0);
        assert_eq!(s.max, 5.0);
        assert_eq!(s.mad, 2.0);
    }

    #[test]
    fn even_count() {
        let mut v = vec![1.0, 2.0, 3.0, 4.0];
        let s = summarize(&mut v);
        assert_eq!(s.median, 2.5);
        assert_eq!(s.mad, 1.0);
    }

    #[test]
    fn constant_samples_have_zero_spread() {
        let mut v = vec![7.0; 31];
        let s = summarize(&mut v);
        assert_eq!(s.median, 7.0);
        assert_eq!(s.mad, 0.0);
        assert_eq!(s.min, s.max);
    }
}
