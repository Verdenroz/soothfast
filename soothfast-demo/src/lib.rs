//! soothfast's own measurement primitives, bound into other languages.
//!
//! The dogfood for `cargo soothfast bind`: one annotated Rust surface that
//! `[[bind]]` turns into a Python package and a JavaScript one. What it
//! exports is what soothfast gates on internally, so the bindings are worth
//! having rather than a toy.

use soothfast_measure::stats;

/// Which statistic a comparison reads.
#[soothfast::export]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Median,
    Mad,
    Min,
    Max,
}

/// A robust summary of one sample set.
///
/// Median and MAD rather than mean and standard deviation, so a single
/// scheduler blip cannot move the numbers a gate compares.
#[soothfast::export]
pub struct Summary {
    pub median: f64,
    /// Median absolute deviation, unscaled.
    pub mad: f64,
    pub min: f64,
    pub max: f64,
}

#[soothfast::export]
impl Summary {
    /// Summarize a sample set. Fails on an empty one, which has no median.
    pub fn new(samples: Vec<f64>) -> Result<Self, String> {
        if samples.is_empty() {
            return Err("cannot summarize an empty sample set".into());
        }
        if samples.iter().any(|v| v.is_nan()) {
            return Err("cannot summarize samples containing NaN".into());
        }
        let mut samples = samples;
        let summary = stats::summarize(&mut samples);
        Ok(Summary {
            median: summary.median,
            mad: summary.mad,
            min: summary.min,
            max: summary.max,
        })
    }

    /// Read one statistic by name.
    pub fn get(&self, metric: Metric) -> f64 {
        match metric {
            Metric::Median => self.median,
            Metric::Mad => self.mad,
            Metric::Min => self.min,
            Metric::Max => self.max,
        }
    }

    /// How far `value` sits from the median, in MAD units.
    ///
    /// Zero MAD means every sample was identical, so anything other than
    /// the median is infinitely far from it.
    pub fn deviations(&self, value: f64) -> f64 {
        if self.mad == 0.0 {
            return if value == self.median {
                0.0
            } else {
                f64::INFINITY
            };
        }
        (value - self.median).abs() / self.mad
    }

    /// How far each of `values` sits from the median, in MAD units.
    ///
    /// The batch form of [`Summary::deviations`]: one crossing for the whole
    /// series rather than one per value. Borrowing the input rather than
    /// taking it by value is what lets a caller hand over a buffer instead of
    /// a boxed sequence.
    pub fn deviations_all(&self, values: &[f64]) -> Vec<f64> {
        values.iter().map(|v| self.deviations(*v)).collect()
    }

    /// [`Summary::deviations_all`], writing into a caller-owned buffer.
    ///
    /// Neither side is copied, which is the only shape that keeps the whole
    /// call off the boxing path.
    pub fn deviations_into(&self, values: &[f64], out: &mut [f64]) {
        for (value, slot) in values.iter().zip(out.iter_mut()) {
            *slot = self.deviations(*value);
        }
    }

    /// Parse a comma-separated sample set.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut samples = Vec::new();
        for field in text.split(',').map(str::trim).filter(|f| !f.is_empty()) {
            match field.parse::<f64>() {
                Ok(value) => samples.push(value),
                Err(_) => return Err(format!("`{field}` is not a number")),
            }
        }
        Summary::new(samples)
    }

    /// A one-line rendering of the whole summary.
    pub fn label(&self) -> String {
        format!(
            "median {} mad {} min {} max {}",
            self.median, self.mad, self.min, self.max
        )
    }

    /// Scale every statistic by `factor`.
    pub fn rescale(&mut self, factor: f64) {
        self.median *= factor;
        self.mad *= factor.abs();
        self.min *= factor;
        self.max *= factor;
        if factor < 0.0 {
            std::mem::swap(&mut self.min, &mut self.max);
        }
    }
}

/// FNV-1a fingerprint of some bytes.
///
/// The frozen hash soothfast records in baselines and lockfiles, so a
/// consumer in another language can recompute one soothfast would accept.
#[soothfast::export]
pub fn fingerprint(bytes: Vec<u8>) -> u64 {
    soothfast_registry::fnv1a(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_reads_the_same_through_get_as_through_its_fields() {
        let s = Summary::new(vec![3.0, 1.0, 5.0]).expect("summarizes");
        assert_eq!(s.median, 3.0);
        assert_eq!(s.get(Metric::Median), s.median);
        assert_eq!(s.get(Metric::Min), 1.0);
        assert_eq!(s.get(Metric::Max), 5.0);
    }

    #[test]
    fn an_empty_sample_set_has_no_median_to_report() {
        assert!(Summary::new(Vec::new()).is_err());
    }

    #[test]
    fn nan_is_refused_rather_than_sorted_wrongly() {
        assert!(Summary::new(vec![1.0, f64::NAN]).is_err());
    }

    #[test]
    fn identical_samples_put_everything_else_infinitely_far_away() {
        let s = Summary::new(vec![2.0, 2.0, 2.0]).expect("summarizes");
        assert_eq!(s.deviations(2.0), 0.0);
        assert!(s.deviations(3.0).is_infinite());
    }

    #[test]
    fn the_batch_form_agrees_with_the_single_one() {
        let s = Summary::new(vec![1.0, 3.0, 5.0, 7.0]).expect("summarizes");
        let values = vec![0.0, 4.0, 9.0];
        let one_at_a_time: Vec<f64> = values.iter().map(|v| s.deviations(*v)).collect();
        assert_eq!(s.deviations_all(&values), one_at_a_time);

        let mut out = vec![0.0; values.len()];
        s.deviations_into(&values, &mut out);
        assert_eq!(out, one_at_a_time);
    }

    #[test]
    fn every_form_agrees_to_the_last_bit_on_a_ragged_sample() {
        let samples: Vec<f64> = (0..97).map(|i| (i as f64 * 7.3).sin() * 11.7).collect();
        let s = Summary::new(samples.clone()).expect("summarizes");
        let mut out = vec![0.0; samples.len()];
        s.deviations_into(&samples, &mut out);
        for (value, batched) in samples.iter().zip(s.deviations_all(&samples)) {
            assert_eq!(batched.to_bits(), s.deviations(*value).to_bits());
        }
        for (slot, value) in out.iter().zip(&samples) {
            assert_eq!(slot.to_bits(), s.deviations(*value).to_bits());
        }
    }

    #[test]
    fn identical_samples_stay_infinite_through_every_form() {
        let s = Summary::new(vec![2.0, 2.0]).expect("summarizes");
        let mut out = vec![0.0; 2];
        s.deviations_into(&[2.0, 3.0], &mut out);
        assert_eq!(out[0], 0.0);
        assert!(out[1].is_infinite());
        assert_eq!(s.deviations_all(&[2.0, 3.0]), out);
    }

    #[test]
    fn parsing_reads_the_same_samples_the_constructor_would() {
        let parsed = Summary::parse(" 1.0, 3.0 ,5.0, 7.0 ").expect("parses");
        let built = Summary::new(vec![1.0, 3.0, 5.0, 7.0]).expect("summarizes");
        assert_eq!(parsed.median, built.median);
        assert_eq!(parsed.mad, built.mad);
    }

    #[test]
    fn parsing_names_the_field_it_could_not_read() {
        let Err(err) = Summary::parse("1.0,nope,3.0") else {
            panic!("a field that is not a number has to be refused");
        };
        assert!(err.contains("nope"), "{err}");
        assert!(Summary::parse("").is_err());
    }

    #[test]
    fn the_label_carries_every_statistic() {
        let s = Summary::new(vec![1.0, 3.0, 5.0]).expect("summarizes");
        let label = s.label();
        for part in ["median", "mad", "min", "max"] {
            assert!(label.contains(part), "{label}");
        }
    }

    #[test]
    fn rescaling_by_a_negative_factor_swaps_the_ends() {
        let mut s = Summary::new(vec![1.0, 3.0, 5.0]).expect("summarizes");
        s.rescale(-2.0);
        assert_eq!(s.median, -6.0);
        assert_eq!(s.mad, 4.0);
        assert_eq!(s.min, -10.0);
        assert_eq!(s.max, -2.0);
    }

    #[test]
    fn the_fingerprint_matches_the_frozen_reference_vectors() {
        assert_eq!(fingerprint(b"".to_vec()), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fingerprint(b"foobar".to_vec()), 0x8594_4171_f739_67e8);
    }
}
