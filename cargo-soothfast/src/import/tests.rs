use serde_json::json;

use super::*;

const JMH: &str = include_str!("../../tests/fixtures/import/jmh.json");
const JMH_REAL: &str = include_str!("../../tests/fixtures/import/jmh-real.json");

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
            "jmh::com.acme.codec.HashBench.digest[mode=thrpt]",
            "jmh::com.acme.codec.ParseBench.parse[mode=avgt,encoding=utf8,size=1000]",
            "jmh::com.acme.codec.ParseBench.parse[mode=avgt,encoding=utf8,size=10]",
        ]
    );
}

#[test]
fn real_jmh_output_imports_every_mode() {
    let run = imported(Format::Jmh, JMH_REAL);
    let ids: Vec<&str> = run.items.keys().map(String::as_str).collect();
    assert_eq!(
        ids,
        [
            "jmh::bench.SumBench.both[mode=sample,n=1000]",
            "jmh::bench.SumBench.both[mode=sample,n=10]",
            "jmh::bench.SumBench.both[mode=thrpt,n=1000]",
            "jmh::bench.SumBench.both[mode=thrpt,n=10]",
            "jmh::bench.SumBench.sum[mode=avgt,n=1000]",
            "jmh::bench.SumBench.sum[mode=avgt,n=10]",
        ]
    );
    let sampled = &run.items["jmh::bench.SumBench.both[mode=sample,n=10]"];
    assert!(sampled.median_ns.is_some() && sampled.p99_ns.is_some());
    assert!(
        run.items
            .values()
            .all(|m| m.median_ns.is_some_and(|v| v > 0.0))
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
    let us = &run.items["jmh::com.acme.codec.ParseBench.parse[mode=avgt,encoding=utf8,size=1000]"];
    assert_close(us.median_ns, 1255.0);
    assert_close(us.mad_ns, 30.0);
    assert_close(us.p99_ns, 1320.0);
    let ns = &run.items["jmh::com.acme.codec.ParseBench.parse[mode=avgt,encoding=utf8,size=10]"];
    assert_close(ns.median_ns, 140.5);
    assert_close(ns.mad_ns, 3.0);
}

#[test]
fn jmh_throughput_is_inverted_to_time_per_op() {
    let run = imported(Format::Jmh, JMH);
    let digest = &run.items["jmh::com.acme.codec.HashBench.digest[mode=thrpt]"];
    assert_close(digest.median_ns, 500.0);
    assert_close(digest.mad_ns, 11.9);
}

#[test]
fn jmh_sample_mode_reads_its_percentiles() {
    let text = json!([{
        "benchmark": "com.acme.Lat.read",
        "mode": "sample",
        "primaryMetric": {
            "score": 52.0,
            "scoreUnit": "us/op",
            "scorePercentiles": { "50.0": 48.0, "99.0": 90.0 },
            "rawDataHistogram": [[[[48.0, 10]]]]
        }
    }])
    .to_string();
    let run = imported(Format::Jmh, &text);
    let read = &run.items["jmh::com.acme.Lat.read[mode=sample]"];
    assert_close(read.median_ns, 48_000.0);
    assert_close(read.p99_ns, 90_000.0);
}

#[test]
fn jmh_modes_of_one_benchmark_get_distinct_ids() {
    let entry = |mode: &str, unit: &str| {
        json!({
            "benchmark": "b",
            "mode": mode,
            "primaryMetric": {
                "score": 2.0,
                "scoreUnit": unit,
                "scorePercentiles": { "50.0": 2.0 }
            }
        })
    };
    let text = json!([entry("avgt", "ns/op"), entry("thrpt", "ops/s")]).to_string();
    let run = imported(Format::Jmh, &text);
    let ids: Vec<&str> = run.items.keys().map(String::as_str).collect();
    assert_eq!(ids, ["jmh::b[mode=avgt]", "jmh::b[mode=thrpt]"]);
}

#[test]
fn jmh_throughput_percentiles_give_no_p99() {
    let text = json!([{
        "benchmark": "b",
        "mode": "thrpt",
        "primaryMetric": {
            "score": 1000.0,
            "scoreUnit": "ops/s",
            "scorePercentiles": { "50.0": 1000.0, "99.0": 2000.0 }
        }
    }]);
    let run = parse(Format::Jmh, &text.to_string(), None).unwrap();
    let item = &run.items["jmh::b[mode=thrpt]"];
    assert_close(item.median_ns, 1e6);
    assert_eq!(item.p99_ns, None);
}

#[test]
fn jmh_percentiles_without_a_median_name_the_benchmark() {
    let text = json!([{
        "benchmark": "com.acme.Lat.read",
        "mode": "sample",
        "primaryMetric": {
            "score": 52.0,
            "scoreUnit": "us/op",
            "scorePercentiles": { "99.0": 90.0 }
        }
    }]);
    let err = parse(Format::Jmh, &text.to_string(), None).err().unwrap();
    assert!(err.contains("com.acme.Lat.read"), "{err}");
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
        "mode": "avgt",
        "primaryMetric": { "score": 0.0, "scoreUnit": "ops/s" }
    }]);
    assert!(parse(Format::Jmh, &text.to_string(), None).is_err());
}

#[test]
fn duplicate_ids_are_rejected() {
    let one = json!({ "benchmark": "b", "mode": "avgt", "primaryMetric": { "score": 1.0, "scoreUnit": "ns/op" } });
    let text = json!([one, one]).to_string();
    assert!(parse(Format::Jmh, &text, None).is_err());
}

#[test]
fn the_doc_has_the_baseline_shape() {
    let doc = to_doc(&imported(Format::Jmh, JMH));
    assert_eq!(doc["version"], 1);
    assert_eq!(doc["assertions"], json!([]));
    let wall = &doc["items"]["jmh::com.acme.codec.HashBench.digest[mode=thrpt]"]["walltime"];
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
                imported: id.starts_with("jmh::").then(|| "jmh".to_string()),
                ..ItemMetrics::default()
            },
        );
    }
    invoke::save_baseline_in(
        &root,
        "base",
        &earlier,
        invoke::SaveScope::BenchFiltered(None),
    )
    .unwrap();
    invoke::save_baseline_in(&root, "base", &imported(Format::Jmh, JMH), scope()).unwrap();
    let saved = invoke::load_baseline_in(&root, "base").unwrap().unwrap();
    std::fs::remove_dir_all(&root).unwrap();

    assert!(saved["items"]["jmh::stale"].is_null());
    assert!(saved["items"]["pkg::kept"].is_object());
    assert!(saved["items"]["jmh::com.acme.codec.HashBench.digest[mode=thrpt]"].is_object());
}

fn rust_item(ids: &[&str]) -> Run {
    let mut run = Run::default();
    for id in ids {
        let item = ItemMetrics {
            median_ns: Some(1.0),
            ..ItemMetrics::default()
        };
        run.items.insert((*id).into(), item);
    }
    run
}

fn saved_after(name: &str, steps: Vec<(&Run, invoke::SaveScope)>) -> serde_json::Value {
    let root = std::env::temp_dir().join(format!("soothfast-import-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (run, scope) in steps {
        invoke::save_baseline_in(&root, "base", run, scope).unwrap();
    }
    let saved = invoke::load_baseline_in(&root, "base").unwrap().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    saved
}

#[test]
fn a_full_bench_save_keeps_imported_items() {
    let imported = imported(Format::Jmh, JMH);
    let rust = rust_item(&["pkg::new"]);
    for (name, scope) in [
        ("all", invoke::SaveScope::BenchFull(None)),
        ("pkg", invoke::SaveScope::BenchFull(Some("pkg".into()))),
    ] {
        let steps = vec![
            (&imported, invoke::SaveScope::BenchFiltered(None)),
            (&rust, scope),
        ];
        let saved = saved_after(name, steps);
        assert!(saved["items"]["jmh::com.acme.codec.HashBench.digest[mode=thrpt]"].is_object());
        assert!(saved["items"]["pkg::new"].is_object());
    }
}

#[test]
fn a_prefix_equal_to_a_package_name_keeps_its_rust_items() {
    let rust = rust_item(&["soothfast_docs::bench"]);
    let imported = parse(Format::Jmh, JMH, Some("soothfast_docs")).unwrap();
    let scope = invoke::SaveScope::Imported("soothfast_docs".into());
    let steps = vec![
        (&rust, invoke::SaveScope::BenchFiltered(None)),
        (&imported, scope),
    ];
    let saved = saved_after("pkgname", steps);
    assert!(saved["items"]["soothfast_docs::bench"].is_object());
}

#[test]
fn a_prefix_with_separators_replaces_its_stale_entries() {
    let stale = parse(Format::Jmh, JMH, Some("app::bench")).unwrap();
    let mut fresh = parse(Format::Jmh, JMH, Some("app::bench")).unwrap();
    fresh.items.retain(|id, _| id.contains("digest"));
    let scope = invoke::SaveScope::Imported("app::bench".into());
    let steps = vec![
        (&stale, invoke::SaveScope::BenchFiltered(None)),
        (&fresh, scope),
    ];
    let saved = saved_after("sep", steps);
    assert_eq!(saved["items"].as_object().unwrap().len(), 1);
}

const PYPERF: &str = include_str!("../../tests/fixtures/import/pyperf.json");
const PYTEST: &str = include_str!("../../tests/fixtures/import/pytest-benchmark.json");
const VITEST: &str = include_str!("../../tests/fixtures/import/vitest.json");

#[test]
fn pyperf_takes_the_name_from_the_file_and_skips_calibration_runs() {
    let run = imported(Format::Pyperf, PYPERF);
    assert_eq!(run.items.keys().collect::<Vec<_>>(), ["pyperf::timeit"]);
    let timeit = &run.items["pyperf::timeit"];
    assert_close(timeit.median_ns, 6552.200347842429);
    assert_close(timeit.mad_ns, 208.77935791308028);
    assert_close(timeit.p99_ns, 7641.469421448832);
}

#[test]
fn a_pyperf_benchmark_name_wins_over_the_file_name() {
    let text = PYPERF.replace("\"runs\"", "\"metadata\": { \"name\": \"own\" }, \"runs\"");
    let run = imported(Format::Pyperf, &text);
    assert_eq!(run.items.keys().collect::<Vec<_>>(), ["pyperf::own"]);
}

#[test]
fn a_pyperf_unit_other_than_seconds_is_rejected() {
    let text = PYPERF.replace("\"second\"", "\"byte\"");
    let err = parse(Format::Pyperf, &text, None).err().unwrap();
    assert!(err.contains("\"byte\""), "{err}");
}

#[test]
fn pytest_benchmark_ids_are_fullnames_with_statistics_from_data() {
    let run = imported(Format::Pytest, PYTEST);
    assert_eq!(
        run.items.keys().collect::<Vec<_>>(),
        ["pytest::test_b.py::test_sum"]
    );
    let sum = &run.items["pytest::test_b.py::test_sum"];
    assert_close(sum.median_ns, 6212.001608218998);
    assert_close(sum.mad_ns, 61.00162863731384);
    assert_close(sum.p99_ns, 6903.002940816805);
}

#[test]
fn pytest_benchmark_without_data_approximates_mad_from_stddev() {
    let text = PYTEST.replace("\"data\"", "\"unrelated\"");
    let sum = &imported(Format::Pytest, &text).items["pytest::test_b.py::test_sum"];
    assert_close(sum.median_ns, 6902.999302837998);
    assert_close(sum.mad_ns, 0.0000016219112755701423 * 1e9 * 0.6745);
    assert!(sum.p99_ns.is_none());
}

#[test]
fn vitest_ids_extend_the_file_relative_group_name_and_convert_milliseconds() {
    let run = imported(Format::Vitest, VITEST);
    assert_eq!(
        run.items.keys().collect::<Vec<_>>(),
        [
            "vitest::src/sum.bench.ts > sum > loop",
            "vitest::src/sum.bench.ts > sum > reduce",
        ]
    );
    let looped = &run.items["vitest::src/sum.bench.ts > sum > loop"];
    assert_close(looped.median_ns, 301.0000000358559);
    assert_close(looped.mad_ns, 556.1627924340066);
    assert_close(looped.p99_ns, 550.9999999731008);
}

#[test]
fn vitest_samples_give_exact_statistics() {
    let text = json!({ "files": [{ "groups": [{
        "fullName": "a.bench.ts > g",
        "benchmarks": [{ "name": "n", "samples": [1.0, 2.0, 4.0], "median": 99.0, "sd": 99.0 }]
    }]}]});
    let run = parse(Format::Vitest, &text.to_string(), None).unwrap();
    let n = &run.items["vitest::a.bench.ts > g > n"];
    assert_close(n.median_ns, 2e6);
    assert_close(n.mad_ns, 1e6);
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

#[test]
fn an_import_prefix_naming_a_workspace_crate_or_bench_target_is_refused() {
    let meta = json!({ "packages": [{
        "name": "my-core",
        "targets": [{ "name": "my_core" }, { "name": "soothfast" }],
    }]});
    for clash in ["my_core", "my-core::sub", "soothfast"] {
        let err = clashing_prefix(&[clash.into()], &meta).unwrap_err();
        assert!(err.contains("distinct prefix"), "{clash}: {err}");
    }
    assert!(clashing_prefix(&["jmh".into()], &meta).is_ok());
}
