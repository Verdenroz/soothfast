//! `#[bench(executor = ...)]` expands to glue that builds the executor once,
//! resolves sync or async setup on it, and drives each iteration through it.

use soothfast::Executor;
use soothfast::registry::{Bencher, MeasuredItem, measured_items};
use std::cell::Cell;
use std::thread::LocalKey;

thread_local! {
    static BUILT: Cell<u64> = const { Cell::new(0) };
    static BLOCK_ONS: Cell<u64> = const { Cell::new(0) };
    static RUNS: Cell<u64> = const { Cell::new(0) };
}

fn bump(counter: &'static LocalKey<Cell<u64>>, by: u64) {
    counter.with(|c| c.set(c.get() + by));
}

struct Counting;

impl Executor for Counting {
    fn block_on<F: Future>(&self, fut: F) -> F::Output {
        bump(&BLOCK_ONS, 1);
        soothfast::registry::block_on_counting(fut)
    }
}

fn counting() -> Counting {
    bump(&BUILT, 1);
    Counting
}

async fn async_input() -> u64 {
    41
}

fn sync_input() -> u64 {
    41
}

async fn async_sized(n: usize) -> Vec<u8> {
    vec![0; n]
}

#[soothfast::bench(group = "exec", executor = counting, setup = async_input)]
async fn with_async_setup(n: &u64) {
    bump(&RUNS, *n);
}

#[soothfast::bench(group = "exec", executor = counting, setup = sync_input)]
async fn with_sync_setup(n: &u64) {
    bump(&RUNS, *n);
}

#[soothfast::bench(group = "exec", executor = counting, setup_sized = async_sized,
                   sizes(8, 16), complexity = "n")]
async fn with_sweep(v: &[u8]) {
    bump(&RUNS, v.len() as u64);
}

#[soothfast::measured(group = "exec", executor = counting)]
async fn measured_on_executor() {}

fn item(name: &str) -> &'static MeasuredItem {
    measured_items().iter().find(|i| i.name == name).unwrap()
}

fn counts() -> (u64, u64, u64) {
    (BUILT.get(), BLOCK_ONS.get(), RUNS.get())
}

#[test]
fn async_setup_resolves_on_the_executor() {
    let mut collector = |body: &mut dyn FnMut()| body();
    (item("with_async_setup").runner)(&mut Bencher::__new(3, &mut collector));
    assert_eq!(counts(), (1, 1 + 3, 41 * 3));
}

#[test]
fn sync_setup_passes_through() {
    let mut collector = |body: &mut dyn FnMut()| body();
    (item("with_sync_setup").runner)(&mut Bencher::__new(2, &mut collector));
    assert_eq!(counts(), (1, 2, 41 * 2));
}

#[test]
fn executor_items_skip_the_counting_backend() {
    assert!(!item("with_async_setup").is_async);
    assert!(!item("measured_on_executor").is_async);
}

#[test]
fn sweep_resolves_async_sized_setup_on_the_executor() {
    let it = item("with_sweep");
    assert_eq!(it.sizes, &[8, 16]);
    let mut collector = |body: &mut dyn FnMut()| body();
    (it.sized_runner.unwrap())(&mut Bencher::__new(2, &mut collector), 16);
    assert_eq!(counts(), (1, 1 + 2, 16 * 2));
}
