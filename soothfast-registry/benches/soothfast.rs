//! The registry measured by soothfast: fingerprint hashing is the hot path
//! every `docs check` and `measure` run leans on.

use soothfast::{bench, fixture, keep};

soothfast::bench_main!();

/// Deterministic pseudo-random bytes (seeded LCG, no rand dep).
#[fixture]
fn bytes_n(n: usize) -> Vec<u8> {
    let mut x: u64 = 0x5DEE_CE66_D111_1111;
    (0..n)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u8
        })
        .collect()
}

/// The frozen fingerprint hash: linear, zero-alloc — the registry's pulse.
#[bench(
    group = "self",
    setup_sized = bytes_n,
    sizes(1024, 8192, 65536),
    complexity = "n",
    alloc = 0,
    covers = "soothfast_registry::fnv1a"
)]
fn bench_fnv1a(input: &[u8]) {
    keep(soothfast_registry::fnv1a(keep(input)));
}
