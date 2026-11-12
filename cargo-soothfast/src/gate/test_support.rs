//! Test fixtures shared across the gate submodules.

use serde_json::{Value, json};

use super::args::{GateArgs, ImportArgs};
use super::compare::{CompareCtx, compare};
use super::imported::run_imported;
use super::resolve::{Resolved, assertion_failures};
use crate::gate_lock;
use crate::invoke::{self, CommonArgs, ItemMetrics, Run};
use crate::test::fixture::TempDir;

pub(super) fn ctx() -> CompareCtx {
    CompareCtx {
        deps_mode: false,
        allow_gone: false,
        filter: None,
        shard: None,
        pkg: None,
        buildcost_mode: false,
        accepted: gate_lock::Entries::new(),
        headroom_pct: None,
        walltime_advisory: false,
    }
}

pub(super) fn reference(item: Value) -> Value {
    json!({ "version": 1, "noise_pct": 0.20, "items": { "pkg::bt_base_to_htf_index": item } })
}

pub(super) fn run_with(m: ItemMetrics) -> Run {
    let mut run = Run::default();
    run.items.insert("pkg::bt_base_to_htf_index".into(), m);
    run
}

pub(super) fn gate_failures(reference: &Value, current: &Run) -> (u32, Vec<String>) {
    gate_failures_with(reference, current, ctx())
}

pub(super) fn gate_failures_with(
    reference: &Value,
    current: &Run,
    ctx: CompareCtx,
) -> (u32, Vec<String>) {
    let mut failing = Vec::new();
    let failures = compare(
        reference,
        current,
        "",
        &ctx,
        &mut failing,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    (failures, failing)
}

pub(super) fn reused(mut doc: Value) -> Value {
    doc["reused_from"] = json!("deadbeef");
    doc
}

/// A new crate's run whose one alloc claim holds or not.
pub(super) fn new_crate_run(holds: bool) -> Run {
    let mut head = run_with(ItemMetrics {
        allocs: Some(0),
        bytes: Some(0),
        ..ItemMetrics::default()
    });
    head.assertions.push(invoke::AssertionOutcome {
        id: "pkg::bt_base_to_htf_index".into(),
        kind: "alloc".into(),
        ok: holds,
        detail: "claimed 0".into(),
    });
    head
}

pub(super) fn gated(resolved: &Resolved) -> u32 {
    let (regressions, _) = gate_failures(&resolved.reference, &resolved.current);
    regressions + assertion_failures(&resolved.current, &mut Vec::new())
}

pub(super) fn scratch(name: &str) -> TempDir {
    TempDir::new(name)
}

pub(super) fn pyperf(seconds: f64) -> String {
    pyperf_spread(seconds, 0.0)
}

pub(super) fn pyperf_spread(seconds: f64, spread: f64) -> String {
    let (lo, hi) = (seconds * (1.0 - spread), seconds * (1.0 + spread));
    let runs = json!([{ "values": [lo, seconds, hi] }]);
    json!({ "benchmarks": [{ "metadata": { "name": "b" }, "runs": runs }] }).to_string()
}

pub(super) fn imported_gate_with(
    name: &str,
    current: f64,
    config: &str,
    baseline: bool,
    tweak: impl FnOnce(&mut GateArgs),
) -> i32 {
    let root = std::env::temp_dir().join(format!("soothfast-gate-import-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("soothfast.toml"), config).unwrap();
    let file = |stem: &str, text: String| {
        let path = root.join(stem);
        std::fs::write(&path, text).unwrap();
        ("pyperf".to_string(), path.display().to_string())
    };
    let reference = file("ref.json", pyperf(0.001));
    let mut g = GateArgs {
        common: CommonArgs::default(),
        baseline: "base".into(),
        ratchet: None,
        against_ref: None,
        deps: false,
        allow_gone: false,
        allow_harness_change: false,
        matrix: "default".into(),
        save_baseline: None,
        reuse_base: true,
        measure_new: true,
        import: ImportArgs {
            current: Some(file("cur.json", pyperf(current))),
            reference: Some(reference.clone()),
            prefix: None,
            walltime: None,
            baseline_given: false,
        },
    };
    tweak(&mut g);
    if baseline {
        let (run, prefix) = crate::import::read(&reference.0, &reference.1, None).unwrap();
        let scope = invoke::SaveScope::Imported(prefix);
        invoke::save_baseline_in(&root, "base", &run, scope).unwrap();
        let other = ItemMetrics {
            median_ns: Some(1.0),
            imported: Some("jmh".into()),
            ..ItemMetrics::default()
        };
        let mut other_run = Run::default();
        other_run.items.insert("jmh::elsewhere".into(), other);
        let scope = invoke::SaveScope::Imported("jmh".into());
        invoke::save_baseline_in(&root, "base", &other_run, scope).unwrap();
        g.import.reference = None;
    }
    let code = run_imported(&g, &root);
    let _ = std::fs::remove_dir_all(&root);
    code
}
