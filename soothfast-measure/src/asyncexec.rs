//! Async-behavior backend: polls and wakes per iteration, measured on the
//! registry's counting executor. Catches the regression class instruction
//! counts miss — a future that starts polling twice as often can have
//! identical Ir. Deterministic for well-behaved futures.

use soothfast_registry::{Bencher, async_counters};

/// Per-iteration async behavior for one item.
#[derive(Debug, Clone, Copy)]
pub struct AsyncMeasurement {
    pub polls: u64,
    pub wakes: u64,
}

/// Warm once, then min-of-3 single iterations (same shape as the alloc
/// backend — counts, not times, so min is exact).
pub fn measure(runner: impl Fn(&mut Bencher)) -> AsyncMeasurement {
    let mut best: Option<(u64, u64)> = None;
    let mut collector = |body: &mut dyn FnMut()| {
        body();
        for _ in 0..3 {
            let (p0, w0) = async_counters();
            body();
            let (p1, w1) = async_counters();
            let candidate = (p1 - p0, w1 - w0);
            best = Some(match best {
                None => candidate,
                Some(prev) => (prev.0.min(candidate.0), prev.1.min(candidate.1)),
            });
        }
    };
    runner(&mut Bencher::__new(1, &mut collector));
    let (polls, wakes) = best.expect("runner glue never called Bencher::iter");
    AsyncMeasurement { polls, wakes }
}
