//! Allocation-counting backend: a `GlobalAlloc` wrapper over `System` that
//! counts into process-global atomics. Fully deterministic for
//! single-threaded workloads.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

use soothfast_registry::Bencher;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

/// Counting global allocator; installed by `soothfast::bench_main!`.
pub struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

fn snapshot() -> (u64, u64) {
    (
        ALLOCS.load(Ordering::Relaxed),
        BYTES.load(Ordering::Relaxed),
    )
}

/// Per-iteration allocation counts for one item.
#[derive(Debug, Clone, Copy)]
pub struct AllocMeasurement {
    pub allocs: u64,
    pub bytes: u64,
}

/// Measure one workload iteration's allocations: warm once (lazy statics,
/// allocator pools), then take the minimum over a few single-iteration runs.
pub fn measure(runner: impl Fn(&mut Bencher)) -> AllocMeasurement {
    let mut best: Option<(u64, u64)> = None;
    let mut collector = |body: &mut dyn FnMut()| {
        body();
        for _ in 0..3 {
            let (a0, b0) = snapshot();
            body();
            let (a1, b1) = snapshot();
            let candidate = (a1 - a0, b1 - b0);
            best = Some(match best {
                None => candidate,
                Some(prev) => (prev.0.min(candidate.0), prev.1.min(candidate.1)),
            });
        }
    };
    runner(&mut Bencher::__new(1, &mut collector));
    let (allocs, bytes) = best.expect("runner glue never called Bencher::iter");
    AllocMeasurement { allocs, bytes }
}
