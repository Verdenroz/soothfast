//! Soothfast: measured and documented Rust.
//!
//! Annotate code with `#[measured]` / `#[bench]` / `#[fixture]`; the
//! `cargo-soothfast` CLI runs, gates, and renders everything the annotations
//! register. Adding this crate costs one runtime dependency (`linkme`).
//!
//! Bench wiring (per crate):
//!
//! ```toml
//! [dev-dependencies]
//! soothfast = { version = "...", features = ["runner"] }
//!
//! [[bench]]
//! name = "soothfast"
//! harness = false
//! ```
//!
//! ```no_run
//! # #[cfg(feature = "runner")] {
//! // benches/soothfast.rs
//! soothfast::bench_main!();
//!
//! fn input() -> Vec<f64> { vec![3.0, 1.0, 2.0] }
//! fn sorted(v: &[f64]) -> Vec<f64> {
//!     let mut v = v.to_vec();
//!     v.sort_by(|a, b| a.partial_cmp(b).unwrap());
//!     v
//! }
//!
//! #[soothfast::bench(group = "sort", setup = input, covers = "mylib::sorted")]
//! fn bench_sorted(v: &Vec<f64>) { soothfast::keep(sorted(soothfast::keep(v))); }
//! # }
//! ```

pub mod embed;

pub use soothfast_macros::{bench, fixture, measured, mock_seam, route};
pub use soothfast_registry as registry;

/// Optimizer barrier for measured code, the `black_box` equivalent.
pub fn keep<T>(value: T) -> T {
    std::hint::black_box(value)
}

/// Mock-backend seams for `capture-output`/test examples: activate a
/// `#[soothfast::mock_seam]`-registered backend by name from inside the
/// example itself (mirrors `keep()` — a fn the doc author calls directly,
/// since the markdown `mock=NAME` tag is plain text, not a compile-time
/// reference gentests.rs could splice in for you).
pub mod mock {
    use crate::registry::MockSeam;

    /// Resolve and start the named `#[soothfast::mock_seam]`, passing `arg`
    /// through (`""` when the markdown `mock=NAME` tag carries no `(ARG)`).
    /// Returns a handle whose `base_url()` the example points its client
    /// at; dropping the handle tears the mock backend down.
    ///
    /// # Panics
    /// If no seam (or more than one ambiguous suffix match) is registered
    /// under `name`.
    pub fn activate(name: &str, arg: &str) -> Box<dyn MockSeam> {
        match crate::registry::resolve_mock_seam(crate::registry::mock_seam_items(), name) {
            Ok(item) => (item.setup)(arg),
            Err(msg) => panic!("soothfast::mock::activate: {msg}"),
        }
    }
}

/// Measurement runner, available with the `runner` feature (dev-dependency).
#[cfg(feature = "runner")]
pub mod runner {
    pub use soothfast_measure::{CountingAllocator, main};
}

/// Define the bench binary's `main` and install the counting allocator.
/// Use in `benches/soothfast.rs` with `harness = false`.
#[cfg(feature = "runner")]
#[macro_export]
macro_rules! bench_main {
    () => {
        #[global_allocator]
        static __SOOTHFAST_GLOBAL_ALLOC: $crate::runner::CountingAllocator =
            $crate::runner::CountingAllocator;

        fn main() {
            $crate::runner::main();
        }
    };
}

// Expansion target for soothfast-macros. Not public API.
#[doc(hidden)]
pub mod __private {
    pub use soothfast_registry::{
        Assertions, Bencher, FIXTURES, FixtureItem, MEASURED, MOCKS, MeasuredItem, MockSeam,
        MockSeamItem, ROUTES, RouteItem, fnv1a, linkme,
    };
}
