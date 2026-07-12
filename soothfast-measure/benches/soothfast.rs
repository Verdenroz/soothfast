//! The measurement engine measured by soothfast. Claims carry CI headroom —
//! tighten locally, not the other way round.

use soothfast::{bench, fixture, keep};

soothfast::bench_main!();

/// Deterministic pseudo-random floats in [0, 1).
#[fixture]
fn samples_n(n: usize) -> Vec<f64> {
    let mut x: u64 = 42;
    (0..n)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 11) as f64 / (1u64 << 53) as f64
        })
        .collect()
}

/// Summary stats over one sample set (sorts a clone in place).
/// Measured 4 allocs; claimed with headroom so a std sort change doesn't
/// ring the alarm.
#[bench(
    group = "self",
    setup_sized = samples_n,
    sizes(1024, 4096, 16384),
    complexity = "n log n",
    alloc = 8,
    covers = "soothfast_measure::stats::summarize"
)]
fn bench_summarize(input: &[f64]) {
    let mut v = input.to_vec();
    keep(soothfast_measure::stats::summarize(keep(&mut v)));
}

/// Complexity-sweep verdict math itself: zero-alloc.
#[bench(
    group = "self",
    alloc = 0,
    covers = "soothfast_measure::sweep::evaluate"
)]
fn bench_sweep_evaluate() {
    keep(soothfast_measure::sweep::evaluate(
        keep("n log n"),
        keep(&[1024, 4096, 16384]),
        keep(&[1000.0, 4400.0, 19000.0]),
    ));
}
