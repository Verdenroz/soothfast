//! The demo package measured by soothfast: the Rust-side cost the "Speed"
//! section of docs/bindings.md gates its claims on. A language boundary can
//! only be as cheap as the call it wraps.

use std::cell::RefCell;

use soothfast::{bench, fixture, keep};
use soothfast_demo::{Metric, Summary};

soothfast::bench_main!();

/// Deterministic pseudo-random floats in [0, 1).
#[fixture]
fn samples_n(n: usize) -> Vec<f64> {
    let mut x: u64 = 7;
    (0..n)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 11) as f64 / (1u64 << 53) as f64
        })
        .collect()
}

/// A `Summary` plus a same-sized output buffer, so `deviations_into` never
/// pays for its own scratch space during the timed call.
#[fixture]
fn summary_n(n: usize) -> (Summary, Vec<f64>, RefCell<Vec<f64>>) {
    let values = samples_n(n);
    let summary = Summary::new(values.clone()).expect("summarizes");
    let out = RefCell::new(vec![0.0; n]);
    (summary, values, out)
}

#[fixture]
fn summary() -> Summary {
    Summary::new(vec![3.0, 1.0, 5.0, 2.0, 4.0]).expect("summarizes")
}

/// Building a `Summary` sorts the sample set twice: the boundary cost a
/// caller pays once per handle, not once per read. Measured 4 allocs;
/// claimed with headroom so a std sort change doesn't ring the alarm.
#[bench(
    group = "self",
    setup_sized = samples_n,
    sizes(1024, 4096, 16384),
    complexity = "n log n",
    alloc = 6,
    covers = "soothfast_demo::Summary::new"
)]
fn bench_summary_new(input: &[f64]) {
    keep(Summary::new(keep(input).to_vec()).expect("summarizes"));
}

/// Reading a statistic off an existing handle crosses no conversion at all.
#[bench(
    group = "self",
    setup = summary,
    alloc = 0,
    covers = "soothfast_demo::Summary::get"
)]
fn bench_summary_get(s: &Summary) {
    keep(keep(s).get(Metric::Median));
}

/// The allocating form: one fresh `Vec<f64>` per call.
#[bench(
    group = "self",
    setup_sized = summary_n,
    sizes(1024, 4096, 16384),
    complexity = "n",
    alloc = 1,
    covers = "soothfast_demo::Summary::deviations_all"
)]
fn bench_deviations_all(input: &(Summary, Vec<f64>, RefCell<Vec<f64>>)) {
    let (summary, values, _) = input;
    keep(summary.deviations_all(keep(values)));
}

/// The buffer form: writes into a caller-owned slice, so the call itself
/// allocates nothing.
#[bench(
    group = "self",
    setup_sized = summary_n,
    sizes(1024, 4096, 16384),
    complexity = "n",
    alloc = 0,
    covers = "soothfast_demo::Summary::deviations_into"
)]
fn bench_deviations_into(input: &(Summary, Vec<f64>, RefCell<Vec<f64>>)) {
    let (summary, values, out) = input;
    summary.deviations_into(keep(values), keep(&mut out.borrow_mut()));
}
