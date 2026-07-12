//! The report engine measured by soothfast: `report render` rebuilds
//! `llms.txt` and the perf tables from the surface and the baseline on
//! every docs build.

use serde_json::{Value, json};
use soothfast::{bench, fixture, keep};
use soothfast_report::llms::SurfaceEntry;

soothfast::bench_main!();

/// `n` public items with full doc comments — what `llms.txt` is made of.
#[fixture]
fn entries_n(n: usize) -> Vec<SurfaceEntry> {
    (0..n)
        .map(|i| SurfaceEntry {
            path: format!("bench_crate::module::item_{i}"),
            kind: "function".into(),
            signature: format!("pub fn item_{i}(input: &[u8]) -> u64"),
            pkg: "bench-crate".into(),
            summary: "One line of summary for the item.".into(),
            docs: "One line of summary for the item.\n\nA second paragraph \
                   explaining the usage, which is the whole point of the \
                   digest over a bare signature list.\n"
                .into(),
        })
        .collect()
}

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

/// `llms.txt`: the agent-facing digest, regenerated on every docs build.
#[bench(
    group = "self",
    setup_sized = entries_n,
    sizes(64, 256, 1024),
    complexity = "n",
    covers = "soothfast_report::llms::render"
)]
fn bench_llms_render(entries: &[SurfaceEntry]) {
    let baseline = json!({ "items": {} });
    keep(soothfast_report::llms::render(
        keep("bench-crate"),
        keep(entries),
        &baseline,
    ));
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
