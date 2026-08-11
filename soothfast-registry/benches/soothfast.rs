//! The registry measured by soothfast: fingerprint hashing is the hot path
//! every `docs check` and `measure` run leans on, and the counting executor
//! is what every async measurement is taken on.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

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

/// Pends once, waking itself first so `block_on_counting`'s park is already
/// satisfied when it runs. One yield costs exactly one poll and one wake.
struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            return Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// The counting executor itself, driven by a future that yields a known
/// number of times. Instruction counts cannot see this regression class:
/// a future that starts polling twice as often does the same work per poll.
#[bench(
    group = "self",
    alloc = 1,
    covers = "soothfast_registry::block_on_counting"
)]
async fn bench_yield_chain() {
    for _ in 0..8 {
        keep(YieldOnce(false)).await;
    }
}
