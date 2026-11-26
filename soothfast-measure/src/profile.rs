//! Coverage profile control. A build with `--cfg soothfast_coverage` gives
//! each item's measured region the profile: counters reset right before it
//! and merge into the profile file right after, so code that only runs in
//! setup or in the runner never counts as executed by the bench.

#[cfg(soothfast_coverage)]
pub(crate) use covered::alloc;

#[cfg(not(soothfast_coverage))]
pub(crate) use crate::alloc::measure as alloc;

#[cfg(soothfast_coverage)]
mod covered {
    use soothfast_registry::Bencher;

    use crate::alloc::{self, AllocMeasurement};

    unsafe extern "C" {
        fn __llvm_profile_reset_counters();
        fn __llvm_profile_write_file() -> i32;
    }

    /// The alloc backend over one profiled pass of the item. The pass that
    /// counts allocations runs after the profile is written, and its
    /// counters are dropped.
    pub(crate) fn alloc(runner: impl Fn(&mut Bencher)) -> AllocMeasurement {
        let mut collector = |body: &mut dyn FnMut()| {
            unsafe { __llvm_profile_reset_counters() };
            body();
            unsafe {
                __llvm_profile_write_file();
                __llvm_profile_reset_counters();
            }
        };
        runner(&mut Bencher::__new(1, &mut collector));
        let measurement = alloc::measure(&runner);
        unsafe { __llvm_profile_reset_counters() };
        measurement
    }
}
