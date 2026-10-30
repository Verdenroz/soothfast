//! Keeps `schemas/` in step with what the writers emit.

use serde_json::Value;

use crate::buildstamp::BuildStamp;
use crate::invoke::{AssertionOutcome, ItemMetrics, Reused, Run, SaveScope};
use crate::schema_check;

fn assert_conforms(file: &str, instance: &Value) {
    assert_all_conform(file, std::slice::from_ref(instance));
}

fn assert_all_conform(file: &str, instances: &[Value]) {
    let report = schema_check::check_many(&schema_check::load(file), instances);
    assert!(report.errors.is_empty(), "{file}: {:#?}", report.errors);
    assert!(
        report.unused().is_empty(),
        "{file}: never emitted: {:#?}",
        report.unused()
    );
}

fn full_item() -> ItemMetrics {
    ItemMetrics {
        fingerprint: "00ff".into(),
        covers: "pkg::api".into(),
        median_ns: Some(1.5),
        mad_ns: Some(0.5),
        p99_ns: Some(3.0),
        instructions: Some(10),
        cycles: Some(20),
        cache_refs: Some(30),
        ir: Some(40),
        allocs: Some(1),
        bytes: Some(64),
        polls: Some(2),
        wakes: Some(1),
        tolerance_pct: Some(5.0),
        wall_rounds: vec![1.4, 1.6],
        build_ms: None,
        size_bytes: None,
        reused: Some(Reused {
            from: "item cache".into(),
            metrics: vec!["callgrind"],
        }),
    }
}

fn full_run() -> Run {
    let buildcost = ItemMetrics {
        fingerprint: "11".into(),
        build_ms: Some(900),
        size_bytes: Some(4096),
        ..ItemMetrics::default()
    };
    Run {
        noise_pct: Some(1.2),
        build: Some(BuildStamp {
            rustc: "1.88.0 x86_64-unknown-linux-gnu".into(),
            codegen_units: "1".into(),
            profiles: "abc".into(),
            rustflags: "def".into(),
        }),
        items: [
            ("pkg::bench".to_string(), full_item()),
            ("buildcost::pkg::default".to_string(), buildcost),
        ]
        .into(),
        assertions: vec![AssertionOutcome {
            id: "pkg::bench".into(),
            kind: "alloc".into(),
            ok: true,
            detail: "1 alloc".into(),
        }],
        ..Run::default()
    }
}

struct RemoveOnDrop(std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

#[test]
fn a_saved_baseline_conforms_to_its_schema() {
    let name = format!("schema-drift-{}", std::process::id());
    let path = crate::invoke::save_baseline(&name, &full_run(), SaveScope::BenchFiltered).unwrap();
    let _cleanup = RemoveOnDrop(path.clone());
    let text = std::fs::read_to_string(&path).unwrap();
    assert_conforms(
        "baseline.schema.json",
        &serde_json::from_str(&text).unwrap(),
    );
}

#[test]
fn a_written_lockfile_conforms_to_its_schema() {
    let dir = std::env::temp_dir().join(format!("soothfast-schema-lock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let binds = [("pkg::api".to_string(), "00ff".to_string())].into();
    soothfast_docs::lockfile::write(&dir, &binds).unwrap();
    let text = std::fs::read_to_string(dir.join(soothfast_docs::lockfile::LOCKFILE)).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_conforms("lock.schema.json", &serde_json::from_str(&text).unwrap());
}

#[test]
fn runner_records_conform_to_the_record_schema() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/schema_samples/runner-records.jsonl");
    let records: Vec<Value> = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_all_conform("runner-record.schema.json", &records);
}
