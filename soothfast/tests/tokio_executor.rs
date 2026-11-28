//! A tokio runtime drives bench futures that need its timer.
#![cfg(feature = "tokio")]

use soothfast::registry::{Bencher, measured_items};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static SLEPT: AtomicU64 = AtomicU64::new(0);

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

#[soothfast::bench(executor = runtime)]
async fn sleeps_on_tokio() {
    tokio::time::sleep(Duration::from_millis(1)).await;
    SLEPT.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn runtime_drives_a_timer_bound_future() {
    let item = measured_items()
        .iter()
        .find(|i| i.name == "sleeps_on_tokio")
        .unwrap();
    let mut collector = |body: &mut dyn FnMut()| body();
    (item.runner)(&mut Bencher::__new(3, &mut collector));
    assert_eq!(SLEPT.load(Ordering::SeqCst), 3);
}
