use super::*;
use crate::invoke::{AssertionOutcome, ItemMetrics};
use serde_json::{Value, json};

fn scratch(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("soothfast-save-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn core() -> CommonArgs {
    CommonArgs {
        pkg: Some("core".into()),
        ..CommonArgs::default()
    }
}

fn item(reused: Option<Reused>) -> ItemMetrics {
    ItemMetrics {
        median_ns: Some(10.0),
        mad_ns: Some(1.0),
        p99_ns: Some(12.0),
        ir: Some(900),
        allocs: Some(0),
        bytes: Some(0),
        reused,
        ..ItemMetrics::default()
    }
}

fn verdict(id: &str, kind: &str) -> AssertionOutcome {
    AssertionOutcome {
        id: id.into(),
        kind: kind.into(),
        ok: true,
        detail: String::new(),
    }
}

fn saved(name: &str, run: &Run, source: &HeadRun, failures: u32) -> Value {
    let root = scratch(name);
    let listed = ["core::a".to_string(), "core::b".to_string()];
    save_head(&root, "head", run, source, &core(), Some(&listed), failures).unwrap();
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    let _ = std::fs::remove_dir_all(&root);
    doc
}

fn run_of(items: Vec<(&str, ItemMetrics)>) -> Run {
    let mut run = Run::default();
    for (id, m) in items {
        run.items.insert(id.into(), m);
    }
    run
}

#[test]
fn a_failing_gate_saves_no_numbers_and_names_its_items() {
    let run = run_of(vec![("core::a", item(None))]);
    let doc = saved("failed", &run, &HeadRun::Measured, 1);
    assert_eq!(doc["items"], json!({}));
    let entries = unmeasured::read(&doc).unwrap();
    assert_eq!(entries[0].reason, Reason::GateFailed);
    assert_eq!(entries[0].items, ["core::a", "core::b"]);
}

#[test]
fn a_run_from_the_run_cache_keeps_its_counters_and_drops_its_walltime() {
    let run = run_of(vec![("core::a", item(None))]);
    let doc = saved("stored", &run, &HeadRun::Stored("abc".into()), 0);
    let a = &doc["items"]["core::a"];
    assert_eq!(a["callgrind"]["ir"], 900);
    assert!(a.get("walltime").is_none());
    assert_eq!(a["reused"]["from"], "abc");
    let entries = unmeasured::read(&doc).unwrap();
    assert_eq!(entries[0].reason, Reason::ReusedWalltime);
    assert_eq!(entries[0].metrics, ["walltime"]);
}

#[test]
fn an_item_spliced_from_the_item_cache_drops_only_its_walltime_and_verdicts() {
    let spliced = Reused {
        from: "item cache".into(),
        metrics: vec!["callgrind.ir", WALLTIME],
    };
    let mut run = run_of(vec![
        ("core::a", item(Some(spliced))),
        ("core::b", item(None)),
    ]);
    run.assertions = vec![verdict("core::a", "p99"), verdict("core::b", "complexity")];
    let doc = saved("spliced", &run, &HeadRun::Measured, 0);
    assert!(doc["items"]["core::a"].get("walltime").is_none());
    assert_eq!(
        doc["items"]["core::a"]["reused"]["metrics"],
        json!(["callgrind.ir"])
    );
    assert_eq!(doc["items"]["core::b"]["walltime"]["median_ns"], 10.0);
    let kept: Vec<&str> = doc["assertions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["id"].as_str())
        .collect();
    assert_eq!(kept, ["core::b"]);
}

#[test]
fn an_uncounted_pass_saves_its_own_walltime_and_lists_the_counters() {
    let mut m = item(None);
    m.ir = None;
    let mut run = run_of(vec![("core::a", m)]);
    run.assertions = vec![
        verdict("core::a", "complexity"),
        verdict("core::a", "alloc"),
    ];
    let doc = saved("uncounted", &run, &HeadRun::Uncounted, 0);
    assert_eq!(doc["items"]["core::a"]["walltime"]["median_ns"], 10.0);
    let entries = unmeasured::read(&doc).unwrap();
    assert_eq!(entries[0].reason, Reason::IdenticalBinaries);
    assert_eq!(entries[0].metrics, ["perfcnt", "callgrind"]);
    assert_eq!(doc["assertions"][0]["kind"], "alloc");
    assert_eq!(doc["assertions"].as_array().unwrap().len(), 1);
}

#[test]
fn an_errored_gate_names_the_listed_items_as_failed() {
    let root = scratch("errored");
    let listed = ["core::a".to_string(), "other::x".to_string()];
    save_errored(&root, "head", &core(), Some(&listed), "bench failed").unwrap();
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    let entries = unmeasured::read(&doc).unwrap();
    assert_eq!(entries[0].items, ["core::a"]);
    assert!(entries[0].detail.contains("bench failed"));
    let _ = std::fs::remove_dir_all(&root);
}
