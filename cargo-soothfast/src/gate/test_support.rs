//! Test fixtures shared across the gate submodules.

use serde_json::{Value, json};

use super::compare::{CompareCtx, compare};
use crate::gate_lock;
use crate::invoke::{ItemMetrics, Run};

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
