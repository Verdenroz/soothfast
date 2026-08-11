//! Soothfast measurement engine.
//!
//! Zero-dependency (registry aside) by design. Backends: `walltime`
//! (adaptive sampling, median+MAD, A/A noise calibration), `alloc`
//! (counting global allocator), `perfcnt` (Linux CPU counters), `callgrind`,
//! and `asyncexec` (poll/wake counts). Runs inside the user's bench binary
//! via `soothfast::bench_main!` and speaks JSONL on stdout to
//! `cargo-soothfast`.

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
