//! The runtime an exported `async fn` is polled inside.
//!
//! Python's event loop drives the future pyo3 hands it, but it is not a Rust
//! reactor: a future built on tokio finds nothing to register its timers and
//! I/O with, and panics on the first poll. Entering a runtime around each
//! poll gives it one while leaving pyo3's coroutine semantics alone.
//!
//! The waker pyo3 hands that poll takes the GIL when called, and the thread
//! polling is the one holding it. A library thread that wakes a task while
//! holding one of its own locks (h2 wakes a stream from inside its
//! connection lock) then waits for the GIL while the poller waits for that
//! lock. So the glue never hands pyo3's waker to the future: it polls with a
//! waker that only signals a `Notify`, and one runtime task per call relays
//! each signal to pyo3's waker from a context holding nothing.

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
            "tokio = {{ version = \"{TOKIO_VERSION}\", features = [\"rt-multi-thread\", \"sync\"] }}\n"
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
        ::tokio::runtime::Runtime::new()
            .unwrap_or_else(|e| panic!(\"start the binding runtime: {e}\"))
    })
}

/// A future polled inside [`runtime`]'s context.
///
/// The guard is taken per poll rather than held across awaits: it restores a
/// thread-local on drop, and a suspended future would leave that thread
/// carrying a context it never entered.
///
/// The future never sees pyo3's waker, which takes the GIL the polling
/// thread holds: a library waking from under one of its own locks would
/// deadlock against it. It is polled with a waker that only signals
/// `Relay::notify`, and a runtime task relays each signal to pyo3's waker
/// from a context holding no lock at all.
struct OnRuntime<F> {
    inner: F,
    relay: Option<::std::sync::Arc<Relay>>,
}

impl<F> OnRuntime<F> {
    fn new(inner: F) -> Self {
        OnRuntime { inner, relay: None }
    }
}

#[derive(Default)]
struct Relay {
    notify: ::tokio::sync::Notify,
    target: ::std::sync::Mutex<Option<::std::task::Waker>>,
    done: ::std::sync::atomic::AtomicBool,
}

impl ::std::task::Wake for Relay {
    fn wake(self: ::std::sync::Arc<Self>) {
        self.notify.notify_one();
    }

    fn wake_by_ref(self: &::std::sync::Arc<Self>) {
        self.notify.notify_one();
    }
}

impl Relay {
    fn start() -> ::std::sync::Arc<Relay> {
        let relay = ::std::sync::Arc::new(Relay::default());
        let task = ::std::sync::Arc::clone(&relay);
        runtime().spawn(async move {
            loop {
                task.notify.notified().await;
                if task.done.load(::std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                let target = task.target.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some(waker) = target {
                    waker.wake();
                }
            }
        });
        relay
    }
}

impl<F> Drop for OnRuntime<F> {
    fn drop(&mut self) {
        if let Some(relay) = &self.relay {
            relay.done.store(true, ::std::sync::atomic::Ordering::Release);
            relay.notify.notify_one();
        }
    }
}

impl<F: ::std::future::Future> ::std::future::Future for OnRuntime<F> {
    type Output = F::Output;

    fn poll(
        self: ::std::pin::Pin<&mut Self>,
        cx: &mut ::std::task::Context<'_>,
    ) -> ::std::task::Poll<F::Output> {
        // `inner` is never moved out of the wrapper, and `relay` is `Unpin`.
        let this = unsafe { self.get_unchecked_mut() };
        let _guard = runtime().enter();
        let relay = this.relay.get_or_insert_with(Relay::start);
        *relay.target.lock().unwrap_or_else(|e| e.into_inner()) = Some(cx.waker().clone());
        let waker = ::std::task::Waker::from(::std::sync::Arc::clone(relay));
        let mut relayed = ::std::task::Context::from_waker(&waker);
        unsafe { ::std::pin::Pin::new_unchecked(&mut this.inner) }.poll(&mut relayed)
    }
}
"
    .to_string()
}

/// A call awaited inside the runtime.
pub(crate) fn awaited(call: &str) -> String {
    format!("OnRuntime::new({call}).await")
}
