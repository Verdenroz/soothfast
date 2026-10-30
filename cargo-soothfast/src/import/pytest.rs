//! pytest-benchmark `--benchmark-json` output.

use serde_json::Value;

use super::Timing;

/// Parse a pytest-benchmark result, ids named by `fullname`.
///
/// With `--benchmark-save-data` the raw `stats.data` seconds are the samples.
/// Without it only summary stats exist, so the median is `stats.median` and
/// the MAD is approximated as `stats.stddev * 0.6745`, which holds for
/// roughly normal timings only. There is no p99 then.
pub fn parse(text: &str) -> Result<Vec<Timing>, String> {
    let doc: Value =
        serde_json::from_str(text).map_err(|e| format!("not pytest-benchmark JSON: {e}"))?;
    let benchmarks = doc["benchmarks"]
        .as_array()
        .ok_or("pytest-benchmark results need a `benchmarks` array")?;
    benchmarks.iter().map(timing).collect()
}

fn timing(bench: &Value) -> Result<Timing, String> {
    let name = bench["fullname"]
        .as_str()
        .ok_or("pytest-benchmark entry without `fullname`")?
        .to_string();
    let stats = &bench["stats"];
    if let Some(data) = stats["data"].as_array().filter(|d| !d.is_empty()) {
        let samples = data
            .iter()
            .filter_map(Value::as_f64)
            .map(|s| s * 1e9)
            .collect();
        return Timing::from_samples(name, samples);
    }
    let field = |key: &str| {
        stats[key]
            .as_f64()
            .map(|s| s * 1e9)
            .ok_or_else(|| format!("{name}: no stats.{key}"))
    };
    Ok(Timing {
        median_ns: field("median")?,
        mad_ns: field("stddev")? * 0.6745,
        p99_ns: None,
        name,
    })
}
