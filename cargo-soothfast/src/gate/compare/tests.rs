//! Unit tests for the gate compare module.

use serde_json::{Value, json};

use super::{CompareCtx, FailedMetric, compare, reuse_hint};
use crate::buildstamp;
use crate::gate::test_support::{
    ctx, gate_failures, gate_failures_with, reference, reused, run_with,
};
use crate::gate_lock;
use crate::invoke::{ItemMetrics, Run};

fn stamp(profiles: &str) -> buildstamp::BuildStamp {
    buildstamp::BuildStamp {
        rustc: "1.88.0 x86_64-unknown-linux-gnu".into(),
        codegen_units: "1".into(),
        profiles: profiles.into(),
        rustflags: "0".into(),
    }
}

/// 321107 -> 337491 Ir is +5.1%, one instruction per element on a
/// 16384-element workload: a repartitioned codegen unit, not a change.
fn drift_reference(build: Option<Value>) -> Value {
    let mut doc = reference(json!({
        "fingerprint": "fp-unchanged",
        "perfcnt": { "instructions": 321107, "cycles": 91000, "cache_refs": 1200 },
    }));
    if let Some(b) = build {
        doc["build"] = b;
    }
    doc
}

fn drift_head(build: Option<buildstamp::BuildStamp>, tolerance: Option<f64>) -> Run {
    let mut run = run_with(ItemMetrics {
        fingerprint: "fp-unchanged".into(),
        instructions: Some(337491),
        tolerance_pct: tolerance,
        ..Default::default()
    });
    run.build = build;
    run
}

#[test]
fn matching_build_stamps_still_fail_instructions() {
    let reference = drift_reference(Some(stamp("aaaa").to_json()));
    let (failures, failing) = gate_failures(&reference, &drift_head(Some(stamp("aaaa")), None));
    assert_eq!(failures, 1);
    assert_eq!(failing, ["pkg::bt_base_to_htf_index"]);
}

#[test]
fn differing_build_stamps_soften_instructions() {
    let reference = drift_reference(Some(stamp("aaaa").to_json()));
    let (failures, failing) = gate_failures(&reference, &drift_head(Some(stamp("bbbb")), None));
    assert_eq!(failures, 0);
    assert!(failing.is_empty(), "a SOFT item must not enter triage");
}

#[test]
fn reference_without_a_build_stamp_softens() {
    let (failures, failing) = gate_failures(
        &drift_reference(None),
        &drift_head(Some(stamp("aaaa")), None),
    );
    assert_eq!(failures, 0);
    assert!(failing.is_empty());
}

#[test]
fn tolerance_widens_the_instruction_threshold() {
    let reference = drift_reference(Some(stamp("aaaa").to_json()));
    let head = drift_head(Some(stamp("aaaa")), Some(8.0));
    assert_eq!(gate_failures(&reference, &head).0, 0);
}

#[test]
fn tolerance_below_the_delta_still_fails() {
    let reference = drift_reference(Some(stamp("aaaa").to_json()));
    let head = drift_head(Some(stamp("aaaa")), Some(2.0));
    assert_eq!(gate_failures(&reference, &head).0, 1);
}

fn accepted_entries(base_fingerprint: &str, metric: &str, value: f64) -> gate_lock::Entries {
    let mut entries = gate_lock::Entries::new();
    entries.insert(
        "pkg::bt_base_to_htf_index".into(),
        gate_lock::Entry {
            base_fingerprint: base_fingerprint.into(),
            head_fingerprint: "fp-head".into(),
            metrics: [(metric.to_string(), value)].into_iter().collect(),
            justifications: vec![gate_lock::Justification {
                text: "reviewed bug fix".into(),
                accepted_unix: 1,
            }],
        },
    );
    entries
}

#[test]
fn an_accepted_entry_suppresses_a_matching_fail() {
    let reference = drift_reference(Some(stamp("aaaa").to_json()));
    let head = drift_head(Some(stamp("aaaa")), None);
    let ctx = CompareCtx {
        accepted: accepted_entries("fp-unchanged", "instructions", 337491.0),
        ..ctx()
    };
    let (failures, failing) = gate_failures_with(&reference, &head, ctx);
    assert_eq!(failures, 0);
    assert!(failing.is_empty(), "an ACPT item must not enter triage");
}

#[test]
fn a_stale_accepted_entry_still_fails() {
    let reference = drift_reference(Some(stamp("aaaa").to_json()));
    let head = drift_head(Some(stamp("aaaa")), None);
    let ctx = CompareCtx {
        accepted: accepted_entries("fp-old-base", "instructions", 337491.0),
        ..ctx()
    };
    assert_eq!(gate_failures_with(&reference, &head, ctx).0, 1);
}

#[test]
fn a_value_well_below_the_accepted_ceiling_still_passes() {
    // One-sided: the accept bounds an upper ceiling, not a band, so a
    // value far under the accepted number needs no separate re-review.
    let reference = drift_reference(Some(stamp("aaaa").to_json()));
    let head = drift_head(Some(stamp("aaaa")), None);
    let ctx = CompareCtx {
        accepted: accepted_entries("fp-unchanged", "instructions", 500000.0),
        ..ctx()
    };
    assert_eq!(gate_failures_with(&reference, &head, ctx).0, 0);
}

#[test]
fn exceeding_the_accepted_ceiling_still_fails() {
    let reference = drift_reference(Some(stamp("aaaa").to_json()));
    let mut head = drift_head(Some(stamp("aaaa")), None);
    head.items
        .get_mut("pkg::bt_base_to_htf_index")
        .unwrap()
        .instructions = Some(1_000_000);
    let ctx = CompareCtx {
        accepted: accepted_entries("fp-unchanged", "instructions", 337491.0),
        ..ctx()
    };
    assert_eq!(gate_failures_with(&reference, &head, ctx).0, 1);
}

fn walltime_accept_reference() -> Value {
    reference(json!({
        "fingerprint": "fp-unchanged",
        "perfcnt": { "instructions": 100000 },
        "walltime": { "median_ns": 1000.0, "mad_ns": 5.0, "p99_ns": 1100.0 },
    }))
}

#[test]
fn headroom_collects_a_near_miss_metric_within_range() {
    let reference = drift_reference(Some(stamp("aaaa").to_json()));
    let mut head = drift_head(Some(stamp("aaaa")), None);
    // 321107 -> 336520 is +4.8%: under the 5% limit, within 1pt headroom
    // of it, and past the 1.5pt noise floor.
    head.items
        .get_mut("pkg::bt_base_to_htf_index")
        .unwrap()
        .instructions = Some(336520);
    let ctx = CompareCtx {
        headroom_pct: Some(1.0),
        ..ctx()
    };
    let mut out_failed = Vec::new();
    let mut out_near_miss = Vec::new();
    compare(
        &reference,
        &head,
        "",
        &ctx,
        &mut Vec::new(),
        &mut out_failed,
        &mut out_near_miss,
    );
    assert!(out_failed.is_empty());
    assert_eq!(out_near_miss.len(), 1);
    assert_eq!(out_near_miss[0].metric, "instructions");
}

#[test]
fn headroom_excludes_a_delta_within_range_but_under_the_noise_floor() {
    let reference = drift_reference(Some(stamp("aaaa").to_json()));
    let mut head = drift_head(Some(stamp("aaaa")), None);
    // 321107 -> 324318 is +1.0%: within a wide 4.5pt headroom of the 5%
    // limit, but under the 1.5pt noise floor.
    head.items
        .get_mut("pkg::bt_base_to_htf_index")
        .unwrap()
        .instructions = Some(324318);
    let ctx = CompareCtx {
        headroom_pct: Some(4.5),
        ..ctx()
    };
    let mut out_near_miss = Vec::new();
    compare(
        &reference,
        &head,
        "",
        &ctx,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut out_near_miss,
    );
    assert!(out_near_miss.is_empty());
}

#[test]
fn headroom_is_off_by_default() {
    let reference = drift_reference(Some(stamp("aaaa").to_json()));
    let mut head = drift_head(Some(stamp("aaaa")), None);
    head.items
        .get_mut("pkg::bt_base_to_htf_index")
        .unwrap()
        .instructions = Some(336520);
    let mut out_near_miss = Vec::new();
    compare(
        &reference,
        &head,
        "",
        &ctx(),
        &mut Vec::new(),
        &mut Vec::new(),
        &mut out_near_miss,
    );
    assert!(out_near_miss.is_empty());
}

#[test]
fn accepted_counters_soften_walltime_proportionally() {
    let reference = walltime_accept_reference();
    // +25% walltime, within the 17% accepted instructions ratio plus
    // the 10% normal walltime threshold (27% ceiling).
    let head = run_with(ItemMetrics {
        fingerprint: "fp-unchanged".into(),
        median_ns: Some(1250.0),
        ..ItemMetrics::default()
    });
    let ctx = CompareCtx {
        accepted: accepted_entries("fp-unchanged", "instructions", 117000.0),
        ..ctx()
    };
    let (failures, failing) = gate_failures_with(&reference, &head, ctx);
    assert_eq!(failures, 0);
    assert!(failing.is_empty(), "a SOFT item must not enter triage");
}

#[test]
fn walltime_still_fails_past_the_accepted_ceiling() {
    let reference = walltime_accept_reference();
    // +30% walltime, past the 27% ceiling: a real wall-clock pathology
    // the accepted counter growth alone doesn't explain.
    let head = run_with(ItemMetrics {
        fingerprint: "fp-unchanged".into(),
        median_ns: Some(1300.0),
        ..ItemMetrics::default()
    });
    let ctx = CompareCtx {
        accepted: accepted_entries("fp-unchanged", "instructions", 117000.0),
        ..ctx()
    };
    assert_eq!(gate_failures_with(&reference, &head, ctx).0, 1);
}

#[test]
fn out_failed_collects_failing_metrics_for_accept() {
    let reference = drift_reference(Some(stamp("aaaa").to_json()));
    let head = drift_head(Some(stamp("aaaa")), None);
    let mut failing = Vec::new();
    let mut out: Vec<FailedMetric> = Vec::new();
    compare(
        &reference,
        &head,
        "",
        &ctx(),
        &mut failing,
        &mut out,
        &mut Vec::new(),
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].id, "pkg::bt_base_to_htf_index");
    assert_eq!(out[0].metric, "instructions");
    assert_eq!(out[0].new_value, 337491.0);
}

#[test]
fn out_failed_is_empty_when_accepted() {
    let reference = drift_reference(Some(stamp("aaaa").to_json()));
    let head = drift_head(Some(stamp("aaaa")), None);
    let ctx = CompareCtx {
        accepted: accepted_entries("fp-unchanged", "instructions", 337491.0),
        ..ctx()
    };
    let mut failing = Vec::new();
    let mut out: Vec<FailedMetric> = Vec::new();
    compare(
        &reference,
        &head,
        "",
        &ctx,
        &mut failing,
        &mut out,
        &mut Vec::new(),
    );
    assert!(out.is_empty());
}

#[test]
fn a_regression_against_a_reused_perfcnt_reference_names_the_recovery() {
    let reused = json!({ "reused_from": "1aa6c4de", "items": {} });
    assert_eq!(
        reuse_hint(&reused, Some("perfcnt"), 1).as_deref(),
        Some("gate: reference reused from 1aa6c4de; rerun with --no-reuse-base to re-measure")
    );
}

#[test]
fn no_reuse_hint_without_a_reused_perfcnt_regression() {
    let reused = json!({ "reused_from": "1aa6c4de", "items": {} });
    assert_eq!(reuse_hint(&reused, Some("perfcnt"), 0), None);
    assert_eq!(reuse_hint(&reused, Some("callgrind"), 1), None);
    assert_eq!(
        reuse_hint(&json!({ "items": {} }), Some("perfcnt"), 1),
        None
    );
}

fn walltime_reference() -> Value {
    reference(json!({
        "fingerprint": "fp-base",
        "walltime": { "median_ns": 1000.0, "mad_ns": 5.0, "p99_ns": 1100.0 },
    }))
}

fn walltime_head() -> Run {
    run_with(ItemMetrics {
        fingerprint: "fp-head".into(),
        median_ns: Some(1300.0),
        ..Default::default()
    })
}

#[test]
fn a_freshly_measured_reference_fails_walltime() {
    assert_eq!(gate_failures(&walltime_reference(), &walltime_head()).0, 1);
}

#[test]
fn a_reused_reference_softens_walltime() {
    let (failures, failing) = gate_failures(&reused(walltime_reference()), &walltime_head());
    assert_eq!(failures, 0);
    assert!(failing.is_empty());
}

#[test]
fn a_vanished_item_fails_only_in_the_shard_that_owns_it() {
    let gone =
        json!({ "version": 1, "items": { "pkg::gone": {}, "pkg::bt_base_to_htf_index": {} } });
    let head = run_with(ItemMetrics::default());
    let failures: u32 = (1..=3)
        .map(|index| {
            let ctx = CompareCtx {
                shard: Some(crate::invoke::Shard { index, count: 3 }),
                ..ctx()
            };
            gate_failures_with(&gone, &head, ctx).0
        })
        .sum();
    assert_eq!(failures, 1);
}

#[test]
fn an_item_reused_from_the_store_softens_its_walltime() {
    let mut doc = walltime_reference();
    doc["items"]["pkg::bt_base_to_htf_index"]["reused_from"] = json!("item cache");
    assert_eq!(gate_failures(&doc, &walltime_head()).0, 0);
    let mut head = walltime_head();
    for item in head.items.values_mut() {
        item.reused = Some(crate::invoke::Reused {
            from: "item cache".into(),
            metrics: Vec::new(),
        });
    }
    assert_eq!(gate_failures(&walltime_reference(), &head).0, 0);
}

#[test]
fn advisory_walltime_never_fails() {
    let ctx = CompareCtx {
        walltime_advisory: true,
        ..ctx()
    };
    let (failures, failing) = gate_failures_with(&walltime_reference(), &walltime_head(), ctx);
    assert_eq!(failures, 0);
    assert!(failing.is_empty(), "a SOFT item must not enter triage");
}

#[test]
fn advisory_walltime_still_fails_counters() {
    let ctx = CompareCtx {
        walltime_advisory: true,
        ..ctx()
    };
    let mut head = incident_head();
    head.instructions = Some(400_000);
    let (failures, failing) =
        gate_failures_with(&incident_reference("fp-old"), &run_with(head), ctx);
    assert_eq!(failures, 1, "instructions still gate; walltime is SOFT");
    assert_eq!(failing, vec!["pkg::bt_base_to_htf_index"]);
}

#[test]
fn a_reused_reference_still_fails_instructions() {
    let doc = reused(drift_reference(Some(stamp("aaaa").to_json())));
    let head = drift_head(Some(stamp("aaaa")), None);
    assert_eq!(
        gate_failures(&doc, &head).0,
        1,
        "reuse must not soften counters"
    );
}

/// The motivating incident: walltime 22995 -> 27327 ns (+18.8%) with
/// byte-identical instructions, identical allocs, unchanged fingerprint.
fn incident_reference(fingerprint: &str) -> Value {
    reference(json!({
        "fingerprint": fingerprint,
        "walltime": { "median_ns": 22995.0, "mad_ns": 46.0, "p99_ns": 23500.0 },
        "perfcnt": { "instructions": 337491, "cycles": 91000, "cache_refs": 1200 },
        "alloc": { "allocs": 57, "bytes": 4096 },
    }))
}

fn incident_head() -> ItemMetrics {
    ItemMetrics {
        fingerprint: "fp-unchanged".into(),
        median_ns: Some(27327.0),
        instructions: Some(337491),
        allocs: Some(57),
        bytes: Some(4096),
        ..ItemMetrics::default()
    }
}

#[test]
fn flat_counters_on_unchanged_code_downgrade_a_walltime_fail() {
    let (failures, failing) = gate_failures(
        &incident_reference("fp-unchanged"),
        &run_with(incident_head()),
    );
    assert_eq!(failures, 0);
    assert!(failing.is_empty(), "a SOFT item must not enter triage");
}

#[test]
fn a_walltime_fail_on_changed_code_still_fails() {
    let (failures, failing) =
        gate_failures(&incident_reference("fp-old"), &run_with(incident_head()));
    assert_eq!(failures, 1);
    assert_eq!(failing, vec!["pkg::bt_base_to_htf_index"]);
}

fn rounds_reference(rounds: [f64; 2]) -> Value {
    reference(json!({
        "fingerprint": "fp-old",
        "walltime": { "median_ns": 22995.0, "mad_ns": 46.0, "p99_ns": 23500.0,
                      "rounds": rounds },
        "perfcnt": { "instructions": 337491 },
    }))
}

fn rounds_head(rounds: [f64; 2]) -> ItemMetrics {
    ItemMetrics {
        fingerprint: "fp-new".into(),
        median_ns: Some(rounds[0].min(rounds[1])),
        instructions: Some(337491),
        wall_rounds: rounds.to_vec(),
        ..ItemMetrics::default()
    }
}

#[test]
fn a_single_lucky_base_round_downgrades_a_walltime_fail() {
    // Base round 1 is anomalously fast; pairing 2 shows no regression.
    let (failures, failing) = gate_failures(
        &rounds_reference([22995.0, 27200.0]),
        &run_with(rounds_head([27327.0, 27350.0])),
    );
    assert_eq!(failures, 0);
    assert!(failing.is_empty());
}

#[test]
fn a_regression_in_both_pairings_still_fails() {
    let (failures, _) = gate_failures(
        &rounds_reference([22995.0, 23100.0]),
        &run_with(rounds_head([27327.0, 27350.0])),
    );
    assert_eq!(failures, 1);
}

#[test]
fn a_side_with_one_round_keeps_the_single_comparison() {
    let mut head = rounds_head([27327.0, 27350.0]);
    head.wall_rounds.clear();
    let (failures, _) = gate_failures(&rounds_reference([22995.0, 27200.0]), &run_with(head));
    assert_eq!(failures, 1);
}

#[test]
fn a_walltime_fail_with_moving_counters_still_fails() {
    // +3.7% instructions passes its own +5% gate but breaks the
    // "identical work" corroboration — the slowdown could be real.
    let head = ItemMetrics {
        instructions: Some(350_000),
        ..incident_head()
    };
    let (failures, _) = gate_failures(&incident_reference("fp-unchanged"), &run_with(head));
    assert_eq!(failures, 1);
}

#[test]
fn a_walltime_fail_with_changed_allocs_still_fails() {
    let head = ItemMetrics {
        allocs: Some(58),
        ..incident_head()
    };
    let (failures, _) = gate_failures(&incident_reference("fp-unchanged"), &run_with(head));
    assert_eq!(failures, 1);
}

#[test]
fn no_instruction_counters_means_no_corroboration() {
    let old = reference(json!({
        "fingerprint": "fp-unchanged",
        "walltime": { "median_ns": 22995.0, "mad_ns": 46.0, "p99_ns": 23500.0 },
        "alloc": { "allocs": 57, "bytes": 4096 },
    }));
    let head = ItemMetrics {
        instructions: None,
        ..incident_head()
    };
    let (failures, _) = gate_failures(&old, &run_with(head));
    assert_eq!(failures, 1);
}

/// A ~20K-instruction benchmark's allocator/dynamic-linker churn moves
/// the count past 0.5% while fingerprint/allocs/bytes stay identical.
fn issue93_reference() -> Value {
    reference(json!({
        "fingerprint": "fp-unchanged",
        "walltime": { "median_ns": 1313.2, "mad_ns": 5.0, "p99_ns": 1400.0 },
        "perfcnt": { "instructions": 19965 },
        "alloc": { "allocs": 12, "bytes": 512 },
    }))
}

fn issue93_head(instructions: u64) -> ItemMetrics {
    ItemMetrics {
        fingerprint: "fp-unchanged".into(),
        median_ns: Some(1823.0),
        instructions: Some(instructions),
        allocs: Some(12),
        bytes: Some(512),
        ..ItemMetrics::default()
    }
}

#[test]
fn small_benchmark_instruction_noise_downgrades_a_walltime_fail() {
    // 19965 -> 19859 is -0.53%, past COUNTER_FLAT_PCT alone, but the
    // 106-instruction move is under the absolute floor for a bench this size.
    let (failures, failing) = gate_failures(&issue93_reference(), &run_with(issue93_head(19859)));
    assert_eq!(failures, 0);
    assert!(failing.is_empty(), "a SOFT item must not enter triage");
}

#[test]
fn small_benchmark_floor_does_not_mask_a_real_instruction_change() {
    // 19965 -> 19645 is a 320-instruction, -1.60% move: past both the
    // absolute floor and COUNTER_FLAT_PCT, so it must still corroborate a fail.
    let (failures, _) = gate_failures(&issue93_reference(), &run_with(issue93_head(19645)));
    assert_eq!(failures, 1);
}
