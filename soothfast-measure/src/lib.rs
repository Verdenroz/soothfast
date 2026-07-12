//! Soothfast measurement engine.
//!
//! Zero-dependency (registry aside) by design. Backends
//! implemented in Phase 1: `walltime` (adaptive sampling, median+MAD, A/A
//! noise calibration) and `alloc` (counting global allocator). Runs inside
//! the user's bench binary via `soothfast::bench_main!` and speaks JSONL on
//! stdout to `cargo-soothfast`.

pub mod alloc;
pub mod asyncexec;
pub mod callgrind;
mod json;
#[cfg(target_os = "linux")]
pub mod perfcnt;
pub mod runner;
pub mod stats;
pub mod sweep;
pub mod walltime;

pub use alloc::CountingAllocator;
pub use runner::main;
