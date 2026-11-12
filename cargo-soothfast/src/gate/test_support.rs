//! Test fixtures shared across the gate submodules.

use serde_json::{Value, json};

use super::compare::{CompareCtx, compare};
use super::resolve::{Resolved, assertion_failures};
use crate::gate_lock;
use crate::invoke::{self, ItemMetrics, Run};
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
