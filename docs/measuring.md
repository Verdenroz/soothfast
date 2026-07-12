# Measuring

Annotate a function and it becomes a gated benchmark. This page is dogfood:
the prose is bound to soothfast's own source, the example below compiles and
runs `soothfast-measure`'s production code in CI with its real output spliced
in, and the numbers are checked claims evaluated against this build's own
measurement run.

## Annotate → measured

```rust no_run
use soothfast::{bench, fixture, keep};

soothfast::bench_main!();

#[fixture]
fn samples_n(n: usize) -> Vec<f64> {
    let mut x: u64 = 42;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 11) as f64 / (1u64 << 53) as f64
        })
        .collect()
}

// Checked claims: complexity verified by size sweep, allocations exact.
#[bench(group = "stats", setup_sized = samples_n, sizes(1024, 4096, 16384),
        complexity = "n log n", alloc = 8,
        covers = "soothfast_measure::stats::summarize")]
fn bench_summarize(input: &[f64]) {
    let mut v = input.to_vec();
    keep(soothfast_measure::stats::summarize(keep(&mut v)));
}
```

That attribute is the actual annotation from `soothfast-measure`'s own bench
suite — every soothfast crate carries one and measures itself this way.
`cargo soothfast measure` runs it under every available backend; `cargo
soothfast gate` compares against a baseline or merge-base and exits non-zero
on regression.

## The statistics underneath

<!-- soothfast:bind soothfast_measure::stats::summarize -->
Every walltime measurement is reduced with `summarize`: median and median
absolute deviation, not mean and standard deviation, so a single scheduler
blip cannot drag the summary. It sorts in place and never sees NaN (samples
are durations).
<!-- /soothfast:bind -->

This example runs the production function in CI; the output block below is
its captured stdout, spliced in by `cargo soothfast docs capture` — shown
output can never rot, because it is produced, never written. The `covers=`
tag on the fence attaches the bind above to this exact block, so its lock
state shows up here instead of as a separate marker:

```rust capture-output covers=soothfast_measure::stats::summarize
fn main() {
    let mut samples = vec![9.0, 1.0, 5.0, 3.0, 7.0];
    let s = soothfast_measure::stats::summarize(&mut samples);
    println!("median={} mad={}", s.median, s.mad);
    println!("min={} max={}", s.min, s.max);
}
```

```text soothfast-output
median=5 mad=2
min=1 max=9
```

## Numbers in prose are gated facts

<!-- soothfast:claim soothfast_measure::bench_summarize.alloc.allocs <= 8 -->
Summarizing a sample set costs at most eight allocations.
<!-- /soothfast:claim -->

<!-- soothfast:claim soothfast_registry::bench_fnv1a.alloc.allocs <= 0 -->
Fingerprint hashing allocates nothing at all.
<!-- /soothfast:claim -->

If either sentence stops being true — or the shown output above stops
matching the real one — `cargo soothfast docs check` fails CI: the same
mechanism that keeps every page on this site honest.
