//! Wall-clock backend: adaptive iteration count, warmup, median+MAD summary,
//! and A/A self-calibration of the environment's noise floor.

use std::time::Instant;

use soothfast_registry::Bencher;

use crate::stats::{self, Summary};

/// Aim for ~2ms per sample so timer overhead vanishes into the workload.
const TARGET_SAMPLE_NS: u64 = 2_000_000;
const MAX_ITERS: u64 = 50_000_000;

/// Default sample count; odd so the median is a real observation.
pub const DEFAULT_SAMPLES: u32 = 31;

/// Wall-clock summary for one item, in ns per iteration.
#[derive(Debug, Clone, Copy)]
pub struct WallMeasurement {
    pub summary: Summary,
    /// Nearest-rank 99th percentile (≈ max below ~100 samples).
    pub p99_ns: f64,
    pub samples: u32,
    pub iters: u64,
}

/// Measure one item: pilot run to pick `iters`, then `samples` timed loops.
pub fn measure(runner: impl Fn(&mut Bencher), samples: u32) -> WallMeasurement {
    let mut pilot_ns: u64 = 0;
    {
        let mut collector = |body: &mut dyn FnMut()| {
            body();
            let t = Instant::now();
            body();
            pilot_ns = t.elapsed().as_nanos() as u64;
        };
        runner(&mut Bencher::__new(1, &mut collector));
    }
    // A pilot too fast to time at all needs the maximum iteration count.
    let iters = TARGET_SAMPLE_NS
        .checked_div(pilot_ns)
        .map_or(1_000_000, |n| n.clamp(1, MAX_ITERS));

    let mut per_iter_ns: Vec<f64> = Vec::with_capacity(samples as usize);
    {
        let mut collector = |body: &mut dyn FnMut()| {
            body();
            body();
            for _ in 0..samples {
                let t = Instant::now();
                body();
                per_iter_ns.push(t.elapsed().as_nanos() as f64 / iters as f64);
            }
        };
        runner(&mut Bencher::__new(iters, &mut collector));
    }

    let summary = stats::summarize(&mut per_iter_ns);
    // summarize() sorted in place; nearest-rank p99 on the sorted samples.
    let rank = ((per_iter_ns.len() as f64 * 0.99).ceil() as usize).clamp(1, per_iter_ns.len());
    WallMeasurement {
        summary,
        p99_ns: per_iter_ns[rank - 1],
        samples,
        iters,
    }
}

/// A/A noise calibration: measure an identical reference workload against
/// itself repeatedly; the worst relative disagreement between medians is the
/// environment's observed noise floor (percent). Gates wider than this floor
/// are meaningful; gates inside it are coin flips.
pub fn calibrate(samples: u32) -> f64 {
    fn reference(b: &mut Bencher) {
        b.iter(|| {
            // Seed through black_box or LLVM const-folds the whole chain.
            let mut x: u64 = std::hint::black_box(0x243F_6A88_85A3_08D3);
            for _ in 0..8192 {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
            }
            std::hint::black_box(x)
        });
    }

    let mut worst: f64 = 0.0;
    for _ in 0..3 {
        let a = measure(reference, samples).summary.median;
        let b = measure(reference, samples).summary.median;
        let noise_pct = (a - b).abs() / a.min(b) * 100.0;
        worst = worst.max(noise_pct);
    }
    worst
}
