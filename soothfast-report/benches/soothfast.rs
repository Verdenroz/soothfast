//! The report engine measured by soothfast: `report render` rebuilds
//! perf tables from the baseline on every docs build.

use serde_json::{Value, json};
use soothfast::{bench, fixture, keep};

soothfast::bench_main!();

/// A baseline with `n` measured items, as `measure --save-baseline` writes.
#[fixture]
fn baseline_n(n: usize) -> Value {
    let mut items = serde_json::Map::new();
    for i in 0..n {
        items.insert(
            format!("bench_crate::item_{i}"),
            json!({
                "perfcnt": { "instructions": 1000 + i },
                "walltime": { "median_ns": 250 + i, "p99_ns": 300 + i },
                "alloc": { "allocs": 0 },
            }),
        );
    }
    json!({ "items": Value::Object(items) })
}

/// The perf table rendered into every report page and the changelog draft.
#[bench(
    group = "self",
    setup_sized = baseline_n,
    sizes(64, 256, 1024),
    complexity = "n",
    covers = "soothfast_report::perf_table::markdown"
)]
fn bench_perf_table(baseline: &Value) {
    keep(soothfast_report::perf_table::markdown(keep(baseline)));
}
