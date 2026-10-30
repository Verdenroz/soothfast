//! pyperf `--output` JSON.

use serde_json::Value;

use super::Timing;

/// Parse a pyperf result. Every run's `values` (seconds per loop) are
/// samples; calibration runs carry only warmups and add none.
pub fn parse(text: &str) -> Result<Vec<Timing>, String> {
    let doc: Value = serde_json::from_str(text).map_err(|e| format!("not pyperf JSON: {e}"))?;
    let benchmarks = doc["benchmarks"]
        .as_array()
        .ok_or("pyperf results need a `benchmarks` array")?;
    benchmarks.iter().map(|b| timing(&doc, b)).collect()
}

fn timing(doc: &Value, bench: &Value) -> Result<Timing, String> {
    let name = bench["metadata"]["name"]
        .as_str()
        .or(doc["metadata"]["name"].as_str())
        .ok_or("pyperf benchmark without `metadata.name`")?
        .to_string();
    let unit = bench["metadata"]["unit"]
        .as_str()
        .or(doc["metadata"]["unit"].as_str())
        .unwrap_or("second");
    if unit != "second" {
        return Err(format!("{name}: unknown pyperf unit {unit:?}"));
    }
    let samples = bench["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|run| run["values"].as_array().into_iter().flatten())
        .filter_map(Value::as_f64)
        .map(|s| s * 1e9)
        .collect();
    Timing::from_samples(name, samples)
}
