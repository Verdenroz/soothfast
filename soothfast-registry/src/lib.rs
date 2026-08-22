//! Distributed registry of measured/documented items.
//!
//! Annotations in user code (via `soothfast-macros`) register items into
//! [`linkme`] distributed slices at link time; engines and the CLI read them
//! back through accessor functions. This crate is the only runtime dependency
//! the user-facing surface carries.
//!
//! linkme has no wasm32 support, and nothing reads a registry there anyway:
//! discovery runs on the host. Every slice is absent on wasm32 and every
//! accessor reads empty, so an annotated crate still builds for it.

pub use linkme;

use std::sync::atomic::{AtomicU64, Ordering};

/// Single-use measurement context handed to registered runner glue.
///
/// Generated glue performs setup, then calls [`Bencher::iter`] (or
/// [`Bencher::iter_async`]) exactly once with the workload; the active
/// backend decides how many times (and how) the workload body actually runs.
pub struct Bencher<'a> {
    iters: u64,
    collector: &'a mut dyn FnMut(&mut dyn FnMut()),
}

impl<'a> Bencher<'a> {
    /// Constructor for measurement backends. Not user API.
    #[doc(hidden)]
    pub fn __new(iters: u64, collector: &'a mut dyn FnMut(&mut dyn FnMut())) -> Self {
        Bencher { iters, collector }
    }

    /// Hand the workload to the active backend. Call exactly once per glue call.
    pub fn iter<T>(&mut self, mut f: impl FnMut() -> T) {
        let iters = self.iters;
        (self.collector)(&mut || {
            for _ in 0..iters {
                std::hint::black_box(f());
            }
        });
    }

    /// Async workloads: each iteration drives the future to completion on the
    /// counting executor (polls/wakes feed the `asyncexec` backend).
    pub fn iter_async<F, Fut>(&mut self, mut f: F)
    where
        F: FnMut() -> Fut,
        Fut: Future,
    {
        let iters = self.iters;
        (self.collector)(&mut || {
            for _ in 0..iters {
                std::hint::black_box(block_on_counting(f()));
            }
        });
    }
}

static ASYNC_POLLS: AtomicU64 = AtomicU64::new(0);
static ASYNC_WAKES: AtomicU64 = AtomicU64::new(0);

/// (polls, wakes) since process start — backends diff snapshots around a body.
pub fn async_counters() -> (u64, u64) {
    (
        ASYNC_POLLS.load(Ordering::Relaxed),
        ASYNC_WAKES.load(Ordering::Relaxed),
    )
}

/// Minimal std-only executor with poll/wake counting: enough to measure how
/// a future *behaves* (polls to completion, wake traffic) deterministically.
/// No spawns, no timers — a tokio-instrumented backend is future work.
pub fn block_on_counting<F: Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Wake, Waker};

    struct ParkWaker(std::thread::Thread);
    impl Wake for ParkWaker {
        fn wake(self: std::sync::Arc<Self>) {
            ASYNC_WAKES.fetch_add(1, Ordering::Relaxed);
            self.0.unpark();
        }
        fn wake_by_ref(self: &std::sync::Arc<Self>) {
            ASYNC_WAKES.fetch_add(1, Ordering::Relaxed);
            self.0.unpark();
        }
    }

    let waker = Waker::from(std::sync::Arc::new(ParkWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    loop {
        ASYNC_POLLS.fetch_add(1, Ordering::Relaxed);
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::park(),
        }
    }
}

/// Checked performance claims attached to a measured item.
/// Evaluated by the runner after measurement; violations fail CI.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Assertions {
    /// Maximum allocations per iteration (`alloc = N`; 0 = zero-alloc claim).
    pub max_allocs: Option<u64>,
    /// Tail-latency bound in ns (`p99 = "1ms"`).
    pub p99_ns: Option<u64>,
    /// Claimed complexity class: `"1" | "log n" | "n" | "n log n" | "n^2"`.
    pub complexity: Option<&'static str>,
}

impl Assertions {
    /// Const constructor for macro expansions.
    pub const fn new(
        max_allocs: Option<u64>,
        p99_ns: Option<u64>,
        complexity: Option<&'static str>,
    ) -> Self {
        Assertions {
            max_allocs,
            p99_ns,
            complexity,
        }
    }

    /// No assertions (the default).
    pub const fn none() -> Self {
        Assertions::new(None, None, None)
    }

    pub const fn is_empty(&self) -> bool {
        self.max_allocs.is_none() && self.p99_ns.is_none() && self.complexity.is_none()
    }
}

/// A benchmarked item registered via `#[soothfast::measured]` or `#[soothfast::bench]`.
#[non_exhaustive]
pub struct MeasuredItem {
    /// Package the item was compiled in (`CARGO_PKG_NAME`).
    pub pkg: &'static str,
    /// `module_path!()` at the registration site. In bench/test targets the
    /// first segment is the *target* name, not the crate — see [`Self::full_id`].
    pub module_path: &'static str,
    /// The annotated function's name.
    pub name: &'static str,
    /// Bench group this item reports under.
    pub group: &'static str,
    /// FNV-1a fingerprint of the item's normalized token stream + attr args.
    pub fingerprint: u64,
    /// Public item this measurement covers (for `coverage measure`);
    /// empty means the annotated item itself.
    pub covers: &'static str,
    /// Last path segment of the `setup`/`setup_sized` fn, when one is used;
    /// lets the runner fold the fixture's fingerprint into this item's.
    pub setup: Option<&'static str>,
    /// Generated glue: sets up inputs, then calls `Bencher::iter` once.
    pub runner: fn(&mut Bencher),
    /// Checked performance claims.
    pub assertions: Assertions,
    /// Input sizes for the complexity sweep (empty when no sweep).
    pub sizes: &'static [usize],
    /// Size-parameterized glue (`setup_sized`), used by complexity sweeps.
    pub sized_runner: Option<fn(&mut Bencher, usize)>,
    /// The annotated fn is async (drives the `asyncexec` backend).
    pub is_async: bool,
    /// Gate threshold this item widens to, when the default is too tight for
    /// a body this small.
    pub tolerance_pct: Option<f64>,
}

impl MeasuredItem {
    /// Const constructor — the only way to build one (`#[non_exhaustive]`
    /// forbids literals outside this crate), used by macro expansions.
    pub const fn new(
        pkg: &'static str,
        module_path: &'static str,
        name: &'static str,
        group: &'static str,
        fingerprint: u64,
        covers: &'static str,
        runner: fn(&mut Bencher),
    ) -> Self {
        MeasuredItem {
            pkg,
            module_path,
            name,
            group,
            fingerprint,
            covers,
            setup: None,
            runner,
            assertions: Assertions::none(),
            sizes: &[],
            sized_runner: None,
            is_async: false,
            tolerance_pct: None,
        }
    }

    /// Package-qualified stable ID; survives file moves. In a lib target
    /// `module_path!()` starts with the crate name and the ID reads naturally
    /// (`demo::lcg_checksum`). In bench/test targets the first segment is the
    /// *target* name (every crate's bench file is `soothfast`), which would
    /// collide across packages and clobber shared baselines — substitute the
    /// package name for it.
    pub fn full_id(&self) -> String {
        let pkg_norm = self.pkg.replace('-', "_");
        let (first, rest) = match self.module_path.split_once("::") {
            Some((f, r)) => (f, Some(r)),
            None => (self.module_path, None),
        };
        let mut id = String::with_capacity(self.module_path.len() + self.name.len() + 2);
        if first == pkg_norm {
            id.push_str(self.module_path);
        } else {
            id.push_str(&pkg_norm);
            if let Some(r) = rest {
                id.push_str("::");
                id.push_str(r);
            }
        }
        id.push_str("::");
        id.push_str(self.name);
        id
    }

    /// Record the setup fn's name (const-chainable in static initializers).
    pub const fn with_setup(mut self, setup: &'static str) -> Self {
        self.setup = Some(setup);
        self
    }

    /// Mark the item async (const-chainable in static initializers).
    pub const fn with_async(mut self) -> Self {
        self.is_async = true;
        self
    }

    /// Attach checked claims (const-chainable in static initializers).
    pub const fn with_assertions(mut self, assertions: Assertions) -> Self {
        self.assertions = assertions;
        self
    }

    /// Widen this item's gate threshold (const-chainable in static
    /// initializers).
    pub const fn with_tolerance(mut self, pct: f64) -> Self {
        self.tolerance_pct = Some(pct);
        self
    }

    /// Attach a size sweep (const-chainable in static initializers).
    pub const fn with_sweep(
        mut self,
        sizes: &'static [usize],
        sized_runner: fn(&mut Bencher, usize),
    ) -> Self {
        self.sizes = sizes;
        self.sized_runner = Some(sized_runner);
        self
    }
}

// Manual impl: the HRTB fn-pointer field has no derive-compatible Debug.
impl std::fmt::Debug for MeasuredItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeasuredItem")
            .field("id", &self.full_id())
            .field("group", &self.group)
            .field("fingerprint", &format_args!("{:016x}", self.fingerprint))
            .field("covers", &self.covers)
            .field("assertions", &self.assertions)
            .field("sizes", &self.sizes)
            .finish_non_exhaustive()
    }
}

/// All items registered with `#[soothfast::measured]` / `#[soothfast::bench]`.
#[cfg(not(target_arch = "wasm32"))]
#[linkme::distributed_slice]
pub static MEASURED: [MeasuredItem];

/// Read-only view of every registered measured item.
pub fn measured_items() -> &'static [MeasuredItem] {
    #[cfg(not(target_arch = "wasm32"))]
    return &MEASURED;
    #[cfg(target_arch = "wasm32")]
    &[]
}

/// A declared route/operation registered via `#[soothfast::route]` — the
/// code-side half of spec reconciliation (`cargo soothfast spec check`).
#[derive(Debug)]
#[non_exhaustive]
pub struct RouteItem {
    /// Stable ID of the handler fn: `module_path!() + "::" + name`.
    pub id: &'static str,
    /// Spec file this route is declared in (relative to the package dir).
    pub spec: &'static str,
    /// Operation identity in the spec (operationId / field / tool name).
    pub operation: &'static str,
    /// HTTP verb, or QUERY/MUTATION/SUBSCRIPTION/PUBLISH/SUBSCRIBE/TOOL.
    pub method: &'static str,
    /// Route path / channel / field; empty skips path matching.
    pub path: &'static str,
    /// Request body type name, overriding what the signature implies.
    pub request: Option<&'static str>,
    /// Success response type name. The escape hatch for erased returns
    /// (`impl IntoResponse`), whose concrete type no static analysis can see.
    pub response: Option<&'static str>,
    /// Success status code; `None` means 200, or 204 for a unit return.
    pub status: Option<u16>,
    /// Query-parameter struct name; its fields flatten into query
    /// parameters, overriding what the signature implies.
    pub params: Option<&'static str>,
    /// Path-parameter types: either `"name: Type, name: Type"` pairs naming
    /// individual `{placeholder}`s, or one struct name whose fields do.
    pub path_params: Option<&'static str>,
    /// First line of the annotated fn's doc comment, captured at macro
    /// expansion. Marker fns in bench targets never reach the lib's rustdoc
    /// JSON, so expansion time is the only chance to read it.
    pub summary: Option<&'static str>,
}

impl RouteItem {
    /// Const constructor for macro expansions.
    pub const fn new(
        id: &'static str,
        spec: &'static str,
        operation: &'static str,
        method: &'static str,
        path: &'static str,
    ) -> Self {
        RouteItem {
            id,
            spec,
            operation,
            method,
            path,
            request: None,
            response: None,
            status: None,
            params: None,
            path_params: None,
            summary: None,
        }
    }

    /// Attach the shape overrides. Separate from [`RouteItem::new`] so that
    /// later spec dialects can add their own without growing its arity.
    pub const fn with_shape(
        mut self,
        request: Option<&'static str>,
        response: Option<&'static str>,
        status: Option<u16>,
    ) -> Self {
        self.request = request;
        self.response = response;
        self.status = status;
        self
    }

    /// Attach the query-parameter override. Its own builder for the same
    /// reason [`RouteItem::with_shape`] is separate from `new`.
    pub const fn with_params(mut self, params: Option<&'static str>) -> Self {
        self.params = params;
        self
    }

    /// Attach the path-parameter override, for `{placeholder}`s no signature
    /// types — a detached marker's empty one, chiefly.
    pub const fn with_path_params(mut self, path_params: Option<&'static str>) -> Self {
        self.path_params = path_params;
        self
    }

    /// Attach the expansion-time doc summary.
    pub const fn with_summary(mut self, summary: Option<&'static str>) -> Self {
        self.summary = summary;
        self
    }
}

/// All routes registered with `#[soothfast::route]`.
#[cfg(not(target_arch = "wasm32"))]
#[linkme::distributed_slice]
pub static ROUTES: [RouteItem];

/// Read-only view of every registered route.
pub fn route_items() -> &'static [RouteItem] {
    #[cfg(not(target_arch = "wasm32"))]
    return &ROUTES;
    #[cfg(target_arch = "wasm32")]
    &[]
}

/// An item bound into other languages via `#[soothfast::export]` — the
/// code-side half of native binding generation (`cargo soothfast bind gen`).
#[derive(Debug)]
#[non_exhaustive]
pub struct ExportItem {
    /// Stable ID: `module_path!()`, then the owning type for a method, then
    /// the item name, joined by `::`.
    pub id: &'static str,
    /// What was annotated: `fn`, `method`, `struct`, or `enum`.
    pub kind: &'static str,
    /// FNV-1a fingerprint of the item's normalized token stream.
    pub fingerprint: u64,
    /// Languages this item opts out of, comma-separated. Empty binds every
    /// language the package configures.
    pub skip: &'static str,
    /// Type an associated item belongs to.
    pub owner: Option<&'static str>,
    /// Whether this associated fn builds the type, rather than the inherent
    /// `new` a binding would otherwise pick.
    pub constructor: bool,
    /// First line of the item's doc comment, captured at macro expansion.
    /// Generated glue carries the prose into each language's own docs.
    pub summary: Option<&'static str>,
}

impl ExportItem {
    /// Const constructor for macro expansions.
    pub const fn new(id: &'static str, kind: &'static str, fingerprint: u64) -> Self {
        ExportItem {
            id,
            kind,
            fingerprint,
            skip: "",
            owner: None,
            constructor: false,
            summary: None,
        }
    }

    /// Attach the opt-out language list. Its own builder so that later
    /// backends can add narrowing of their own without growing `new`'s arity.
    pub const fn with_skip(mut self, skip: &'static str) -> Self {
        self.skip = skip;
        self
    }

    /// Attach the owning type of an associated item.
    pub const fn with_owner(mut self, owner: Option<&'static str>) -> Self {
        self.owner = owner;
        self
    }

    /// Mark this associated fn as the type's constructor.
    pub const fn with_constructor(mut self) -> Self {
        self.constructor = true;
        self
    }

    /// Attach the expansion-time doc summary.
    pub const fn with_summary(mut self, summary: Option<&'static str>) -> Self {
        self.summary = summary;
        self
    }
}

/// All items registered with `#[soothfast::export]`.
#[cfg(not(target_arch = "wasm32"))]
#[linkme::distributed_slice]
pub static EXPORTS: [ExportItem];

/// Read-only view of every exported item.
pub fn export_items() -> &'static [ExportItem] {
    #[cfg(not(target_arch = "wasm32"))]
    return &EXPORTS;
    #[cfg(target_arch = "wasm32")]
    &[]
}

/// A deterministic input-builder registered via `#[soothfast::fixture]`.
#[derive(Debug)]
#[non_exhaustive]
pub struct FixtureItem {
    /// Stable ID: `module_path!() + "::" + fn name`.
    pub id: &'static str,
    /// FNV-1a fingerprint of the fixture's normalized token stream. Folded
    /// into dependent items' fingerprints so a workload (input) change can't
    /// masquerade as unchanged code.
    pub fingerprint: u64,
}

impl FixtureItem {
    /// Const constructor for macro expansions.
    pub const fn new(id: &'static str, fingerprint: u64) -> Self {
        FixtureItem { id, fingerprint }
    }
}

/// All fixtures registered with `#[soothfast::fixture]`.
#[cfg(not(target_arch = "wasm32"))]
#[linkme::distributed_slice]
pub static FIXTURES: [FixtureItem];

/// Read-only view of every registered fixture.
pub fn fixture_items() -> &'static [FixtureItem] {
    #[cfg(not(target_arch = "wasm32"))]
    return &FIXTURES;
    #[cfg(target_arch = "wasm32")]
    &[]
}

/// A mocked backend a capture-output/test example can stand up by name.
/// Implemented by a thin consumer-side newtype around whatever mocking
/// crate they bring (mockito, wiremock, a hand-rolled stub server, ...).
/// Teardown is ordinary `Drop` on the concrete type — no separate teardown
/// method is needed.
pub trait MockSeam: Send {
    /// Base URL of the running mock server (e.g. `"http://127.0.0.1:51823"`).
    /// Implementations must bind an OS-assigned port, never a fixed one.
    fn base_url(&self) -> String;
}

/// A mock-backend setup fn registered via `#[soothfast::mock_seam]`.
#[non_exhaustive]
pub struct MockSeamItem {
    /// Stable ID: `module_path!() + "::" + fn name`.
    pub id: &'static str,
    /// FNV-1a fingerprint of the fn's normalized token stream (parallels
    /// `FixtureItem`; unused by lookup, free for future drift tooling).
    pub fingerprint: u64,
    /// Generated glue: calls the annotated fn with the `mock=name(arg)` tag's
    /// arg (`""` when omitted) and boxes its return value.
    pub setup: fn(&str) -> Box<dyn MockSeam>,
}

impl MockSeamItem {
    /// Const constructor for macro expansions.
    pub const fn new(
        id: &'static str,
        fingerprint: u64,
        setup: fn(&str) -> Box<dyn MockSeam>,
    ) -> Self {
        MockSeamItem {
            id,
            fingerprint,
            setup,
        }
    }
}

// Manual impl: the fn-pointer field has no derive-compatible Debug.
impl std::fmt::Debug for MockSeamItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockSeamItem")
            .field("id", &self.id)
            .field("fingerprint", &format_args!("{:016x}", self.fingerprint))
            .finish_non_exhaustive()
    }
}

/// All mock seams registered with `#[soothfast::mock_seam]`.
#[cfg(not(target_arch = "wasm32"))]
#[linkme::distributed_slice]
pub static MOCKS: [MockSeamItem];

/// Read-only view of every registered mock seam.
pub fn mock_seam_items() -> &'static [MockSeamItem] {
    #[cfg(not(target_arch = "wasm32"))]
    return &MOCKS;
    #[cfg(target_arch = "wasm32")]
    &[]
}

/// Resolve a mock seam by name: an exact `id` match wins; otherwise exactly
/// one `::name` suffix match. Pure/testable without linking real
/// `#[mock_seam]`-registered items — `soothfast::mock::activate` calls this
/// against the live `MOCKS` slice and panics with the returned message.
pub fn resolve_mock_seam<'a>(
    items: &'a [MockSeamItem],
    name: &str,
) -> Result<&'a MockSeamItem, String> {
    if let Some(item) = items.iter().find(|m| m.id == name) {
        return Ok(item);
    }
    let suffix = format!("::{name}");
    let matches: Vec<_> = items.iter().filter(|m| m.id.ends_with(&suffix)).collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(format!(
            "no mock seam named {name:?} (missing #[soothfast::mock_seam], or its \
             module isn't compiled under the active feature set)"
        )),
        _ => Err(format!(
            "{name:?} matches {} registered seams; use a fully-qualified module path",
            matches.len()
        )),
    }
}

/// FNV-1a 64-bit hash.
///
/// STABILITY CONTRACT: fingerprints are compared across builds and stored in
/// lockfiles, so this algorithm is frozen. Do not change constants or logic.
pub const fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_known_vectors() {
        // Reference vectors for the frozen algorithm (draft-eastlake-fnv).
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(b"foobar"), 0x8594_4171_f739_67e8);
    }

    struct DummySeam;
    impl MockSeam for DummySeam {
        fn base_url(&self) -> String {
            "http://dummy".into()
        }
    }
    fn dummy_setup(_arg: &str) -> Box<dyn MockSeam> {
        Box::new(DummySeam)
    }

    #[test]
    fn resolve_mock_seam_exact_id_wins_over_suffix() {
        let items = [
            MockSeamItem::new("seam", 0, dummy_setup),
            MockSeamItem::new("other_pkg::seam", 0, dummy_setup),
        ];
        let found = resolve_mock_seam(&items, "seam").unwrap();
        assert_eq!(found.id, "seam");
    }

    #[test]
    fn resolve_mock_seam_unique_suffix_match() {
        let items = [MockSeamItem::new("pkg::mod::seam", 0, dummy_setup)];
        let found = resolve_mock_seam(&items, "seam").unwrap();
        assert_eq!(found.id, "pkg::mod::seam");
    }

    #[test]
    fn resolve_mock_seam_ambiguous_suffix_errors() {
        let items = [
            MockSeamItem::new("pkg_a::seam", 0, dummy_setup),
            MockSeamItem::new("pkg_b::seam", 0, dummy_setup),
        ];
        let err = resolve_mock_seam(&items, "seam").unwrap_err();
        assert!(err.contains("matches 2"), "{err}");
    }

    #[test]
    fn resolve_mock_seam_missing_errors() {
        let items: [MockSeamItem; 0] = [];
        let err = resolve_mock_seam(&items, "seam").unwrap_err();
        assert!(err.contains("no mock seam named"), "{err}");
    }

    #[test]
    fn bencher_runs_body_through_collector() {
        let mut calls = 0;
        let mut collector = |body: &mut dyn FnMut()| {
            body();
            body();
            calls += 1;
        };
        let mut b = Bencher::__new(3, &mut collector);
        let mut work = 0u64;
        b.iter(|| work += 1);
        assert_eq!(calls, 1, "collector invoked once per iter() call");
        assert_eq!(work, 6, "2 body calls x 3 iters");
    }
}
