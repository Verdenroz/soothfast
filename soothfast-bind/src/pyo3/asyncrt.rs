//! The runtime an exported `async fn` is polled inside.
//!
//! Python's event loop drives the future pyo3 hands it, but it is not a Rust
//! reactor: a future built on tokio finds nothing to register its timers and
//! I/O with, and panics on the first poll. Entering a runtime around each
//! poll gives it one while leaving pyo3's coroutine semantics alone.

use crate::plan::BindingPlan;

/// The tokio release the generated glue builds against.
pub(crate) const TOKIO_VERSION: &str = "1";

/// The dependency line an async surface needs, or nothing.
///
/// Cargo unifies this with whatever tokio the bound crate already pulls in,
/// so the runtime entered here is the one that crate's futures expect.
pub(crate) fn dependency(plan: &BindingPlan) -> String {
    match plan.has_async() {
        true => format!(
            "tokio = {{ version = \"{TOKIO_VERSION}\", features = [\"rt-multi-thread\"] }}\n"
        ),
        false => String::new(),
    }
}

/// The runtime and the future wrapper, or nothing when nothing is async.
pub(crate) fn preamble(plan: &BindingPlan) -> String {
    if !plan.has_async() {
        return String::new();
    }
    "
/// The runtime an exported `async fn` is polled inside.
fn runtime() -> &'static ::tokio::runtime::Runtime {
    static RUNTIME: ::std::sync::OnceLock<::tokio::runtime::Runtime> =
        ::std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        ::tokio::runtime::Runtime::new().expect(\"start the binding runtime\")
    })
}

/// A future polled inside [`runtime`]'s context.
///
/// The guard is taken per poll rather than held across awaits: it restores a
/// thread-local on drop, and a suspended future would leave that thread
/// carrying a context it never entered.
struct OnRuntime<F>(F);

impl<F: ::std::future::Future> ::std::future::Future for OnRuntime<F> {
    type Output = F::Output;

    fn poll(
        self: ::std::pin::Pin<&mut Self>,
        cx: &mut ::std::task::Context<'_>,
    ) -> ::std::task::Poll<F::Output> {
        let _guard = runtime().enter();
        // The wrapper is a newtype with no fields of its own, so the inner
        // future is pinned exactly as far as the wrapper is.
        unsafe { self.map_unchecked_mut(|it| &mut it.0) }.poll(cx)
    }
}
"
    .to_string()
}

/// A call awaited inside the runtime.
pub(crate) fn awaited(call: &str) -> String {
    format!("OnRuntime({call}).await")
}
