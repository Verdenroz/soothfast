use serde_json::json;

use super::*;

const JMH: &str = include_str!("../../tests/fixtures/import/jmh.json");

fn imported(format: Format, text: &str) -> Run {
    parse(format, text, None).unwrap()
}

fn assert_close(actual: Option<f64>, expected: f64) {
    let actual = actual.unwrap();
    assert!((actual - expected).abs() < 0.01, "{actual} != {expected}");
}

#[test]
fn jmh_ids_carry_sorted_params() {
    let run = imported(Format::Jmh, JMH);
    let ids: Vec<&str> = run.items.keys().map(String::as_str).collect();
    assert_eq!(
        ids,
        [
            "jmh::com.acme.codec.HashBench.digest",
            "jmh::com.acme.codec.ParseBench.parse[encoding=utf8,size=1000]",
            "jmh::com.acme.codec.ParseBench.parse[encoding=utf8,size=10]",
        ]
    );
}

#[test]
fn a_prefix_replaces_the_format_name() {
    let run = parse(Format::Jmh, JMH, Some("java")).unwrap();
    assert!(run.items.keys().all(|id| id.starts_with("java::")));
}

#[test]
fn jmh_time_units_become_nanoseconds_with_sample_statistics() {
    let run = imported(Format::Jmh, JMH);
    let us = &run.items["jmh::com.acme.codec.ParseBench.parse[encoding=utf8,size=1000]"];
    assert_close(us.median_ns, 1255.0);
    assert_close(us.mad_ns, 30.0);
    assert_close(us.p99_ns, 1320.0);
    let ns = &run.items["jmh::com.acme.codec.ParseBench.parse[encoding=utf8,size=10]"];
    assert_close(ns.median_ns, 140.5);
    assert_close(ns.mad_ns, 3.0);
}

#[test]
fn jmh_throughput_is_inverted_to_time_per_op() {
    let run = imported(Format::Jmh, JMH);
    let digest = &run.items["jmh::com.acme.codec.HashBench.digest"];
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
    let run = parse(Format::Jmh, &text.to_string(), None).unwrap();
    assert_close(run.items["jmh::b"].median_ns, 3e6);
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
    assert!(wall["median_ns"].is_f64() && wall["mad_ns"].is_f64() && wall["p99_ns"].is_f64());
}

#[test]
fn saving_replaces_the_prefix_and_keeps_other_items() {
    let root = std::env::temp_dir().join(format!("soothfast-import-{}", std::process::id()));
    let scope = || invoke::SaveScope::Imported("jmh".into());
    let mut earlier = Run::default();
    for id in ["jmh::stale", "pkg::kept"] {
        earlier.items.insert(
            id.into(),
            ItemMetrics {
                median_ns: Some(1.0),
                ..ItemMetrics::default()
            },
        );
    }
    invoke::save_baseline_in(&root, "base", &earlier, invoke::SaveScope::BenchFiltered).unwrap();
    invoke::save_baseline_in(&root, "base", &imported(Format::Jmh, JMH), scope()).unwrap();
    let saved = invoke::load_baseline_in(&root, "base").unwrap().unwrap();
    std::fs::remove_dir_all(&root).unwrap();

    assert!(saved["items"]["jmh::stale"].is_null());
    assert!(saved["items"]["pkg::kept"].is_object());
    assert!(saved["items"]["jmh::com.acme.codec.HashBench.digest"].is_object());
}

const PYPERF: &str = include_str!("../../tests/fixtures/import/pyperf.json");
const PYTEST: &str = include_str!("../../tests/fixtures/import/pytest-benchmark.json");
const VITEST: &str = include_str!("../../tests/fixtures/import/vitest.json");

#[test]
fn pyperf_values_are_seconds_pooled_across_runs() {
    let run = imported(Format::Pyperf, PYPERF);
    assert_eq!(
        run.items.keys().collect::<Vec<_>>(),
        ["pyperf::json_dumps", "pyperf::json_loads"]
    );
    let loads = &run.items["pyperf::json_loads"];
    assert_close(loads.median_ns, 1125.0);
    assert_close(loads.mad_ns, 75.0);
    assert_close(loads.p99_ns, 1300.0);
}

#[test]
fn a_pyperf_unit_other_than_seconds_is_rejected() {
    let text = PYPERF.replace("\"second\"", "\"byte\"");
    let err = parse(Format::Pyperf, &text, None).err().unwrap();
    assert!(err.contains("\"byte\""), "{err}");
}

#[test]
fn pytest_benchmark_ids_are_fullnames() {
    let run = imported(Format::Pytest, PYTEST);
    assert_eq!(
        run.items.keys().collect::<Vec<_>>(),
        [
            "pytest::tests/test_codec.py::test_decode[large]",
            "pytest::tests/test_codec.py::test_encode",
            "pytest::tests/test_hash.py::test_digest",
        ]
    );
}

#[test]
fn pytest_benchmark_without_data_approximates_mad_from_stddev() {
    let run = imported(Format::Pytest, PYTEST);
    let encode = &run.items["pytest::tests/test_codec.py::test_encode"];
    assert_close(encode.median_ns, 250_000.0);
    assert_close(encode.mad_ns, 13_490.0);
    assert!(encode.p99_ns.is_none());
}

#[test]
fn pytest_benchmark_saved_data_gives_exact_statistics() {
    let run = imported(Format::Pytest, PYTEST);
    let digest = &run.items["pytest::tests/test_hash.py::test_digest"];
    assert_close(digest.median_ns, 1_150_000.0);
    assert_close(digest.mad_ns, 100_000.0);
    assert_close(digest.p99_ns, 1_300_000.0);
}

#[test]
fn vitest_ids_follow_the_printed_hierarchy_and_convert_milliseconds() {
    let run = imported(Format::Vitest, VITEST);
    assert_eq!(
        run.items.keys().collect::<Vec<_>>(),
        [
            "vitest::bench/parse.bench.ts > parse > json",
            "vitest::bench/sort.bench.ts > sort > bubble",
            "vitest::bench/sort.bench.ts > sort > quick",
        ]
    );
    let quick = &run.items["vitest::bench/sort.bench.ts > sort > quick"];
    assert_close(quick.median_ns, 12_000.0);
    assert_close(quick.mad_ns, 400.0);
    assert_close(quick.p99_ns, 20_000.0);
}

#[test]
fn vitest_without_mad_approximates_it_from_sd() {
    let run = imported(Format::Vitest, VITEST);
    let json = &run.items["vitest::bench/parse.bench.ts > parse > json"];
    assert_close(json.mad_ns, 6745.0);
}

#[test]
fn the_cli_names_each_format() {
    for (name, format) in [
        ("jmh", Format::Jmh),
        ("pyperf", Format::Pyperf),
        ("pytest-benchmark", Format::Pytest),
        ("vitest", Format::Vitest),
    ] {
        assert_eq!(Format::parse(name), Ok(format));
    }
    assert!(Format::parse("criterion").is_err());
}
