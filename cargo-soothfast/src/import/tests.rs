use serde_json::json;

use super::*;

const JMH: &str = include_str!("../../tests/fixtures/import/jmh.json");

fn imported(format: Format, text: &str) -> Vec<Bench> {
    parse(format, text, None).unwrap()
}

fn metrics<'a>(benches: &'a [Bench], id: &str) -> &'a ItemMetrics {
    &benches.iter().find(|b| b.id == id).unwrap().metrics
}

fn assert_close(actual: Option<f64>, expected: f64) {
    let actual = actual.unwrap();
    assert!((actual - expected).abs() < 0.01, "{actual} != {expected}");
}

#[test]
fn jmh_ids_carry_sorted_params() {
    let ids: Vec<String> = imported(Format::Jmh, JMH)
        .into_iter()
        .map(|b| b.id)
        .collect();
    assert_eq!(
        ids,
        [
            "jmh::com.acme.codec.ParseBench.parse[encoding=utf8,size=1000]",
            "jmh::com.acme.codec.ParseBench.parse[encoding=utf8,size=10]",
            "jmh::com.acme.codec.HashBench.digest",
        ]
    );
}

#[test]
fn a_prefix_replaces_the_format_name() {
    let benches = parse(Format::Jmh, JMH, Some("java")).unwrap();
    assert!(benches.iter().all(|b| b.id.starts_with("java::")));
}

#[test]
fn jmh_time_units_become_nanoseconds_with_sample_statistics() {
    let benches = imported(Format::Jmh, JMH);
    let us = metrics(
        &benches,
        "jmh::com.acme.codec.ParseBench.parse[encoding=utf8,size=1000]",
    );
    assert_close(us.median_ns, 1255.0);
    assert_close(us.mad_ns, 30.0);
    assert_close(us.p99_ns, 1320.0);
    let ns = metrics(
        &benches,
        "jmh::com.acme.codec.ParseBench.parse[encoding=utf8,size=10]",
    );
    assert_close(ns.median_ns, 140.5);
    assert_close(ns.mad_ns, 3.0);
    assert_eq!(benches[0].samples, 10);
}

#[test]
fn jmh_throughput_is_inverted_to_time_per_op() {
    let benches = imported(Format::Jmh, JMH);
    let digest = metrics(&benches, "jmh::com.acme.codec.HashBench.digest");
    assert_close(digest.median_ns, 500.0);
    assert_close(digest.mad_ns, 11.9);
}

#[test]
fn an_unknown_unit_is_rejected_by_name() {
    let text = JMH.replace("\"us/op\"", "\"fortnights/op\"");
    let err = parse(Format::Jmh, &text, None).err().unwrap();
    assert!(err.contains("\"fortnights/op\""), "{err}");
}

#[test]
fn a_zero_throughput_score_is_rejected() {
    let text = json!([{
        "benchmark": "b",
        "primaryMetric": { "score": 0.0, "scoreUnit": "ops/s" }
    }]);
    assert!(parse(Format::Jmh, &text.to_string(), None).is_err());
}

#[test]
fn a_score_without_raw_data_is_a_single_sample() {
    let text = json!([{
        "benchmark": "b",
        "primaryMetric": { "score": 3.0, "scoreUnit": "ms/op" }
    }]);
    let benches = parse(Format::Jmh, &text.to_string(), None).unwrap();
    assert_close(benches[0].metrics.median_ns, 3e6);
    assert_eq!(benches[0].samples, 1);
}

#[test]
fn duplicate_ids_are_rejected() {
    let one = json!({ "benchmark": "b", "primaryMetric": { "score": 1.0, "scoreUnit": "ns/op" } });
    let text = json!([one, one]).to_string();
    assert!(parse(Format::Jmh, &text, None).is_err());
}

#[test]
fn the_doc_has_the_baseline_shape() {
    let doc = to_doc(&imported(Format::Jmh, JMH));
    assert_eq!(doc["version"], 1);
    assert_eq!(doc["assertions"], json!([]));
    let wall = &doc["items"]["jmh::com.acme.codec.HashBench.digest"]["walltime"];
    assert_eq!(wall["samples"], 4);
    assert!(wall["median_ns"].is_f64() && wall["mad_ns"].is_f64() && wall["p99_ns"].is_f64());
    assert!(wall["rounds"].is_null());
}

#[test]
fn saving_replaces_the_prefix_and_keeps_other_items() {
    let root = std::env::temp_dir().join(format!("soothfast-import-{}", std::process::id()));
    let existing = baseline_file(&root, "base");
    std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
    let before = json!({ "version": 1, "assertions": [], "items": {
        "jmh::stale": { "walltime": { "median_ns": 1.0 } },
        "pkg::kept": { "walltime": { "median_ns": 2.0 } },
    }});
    std::fs::write(&existing, before.to_string()).unwrap();

    let doc = to_doc(&imported(Format::Jmh, JMH));
    save_baseline(&root, "base", "jmh", &doc).unwrap();
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&existing).unwrap()).unwrap();
    std::fs::remove_dir_all(&root).unwrap();

    assert!(saved["items"]["jmh::stale"].is_null());
    assert!(saved["items"]["pkg::kept"].is_object());
    assert!(saved["items"]["jmh::com.acme.codec.HashBench.digest"].is_object());
}
