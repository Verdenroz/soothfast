//! JMH `-rf json` output.

use serde_json::Value;

use super::Timing;

/// Parse a JMH result array. Each benchmark's raw iteration scores are the
/// samples; throughput modes (`ops/s`) are inverted to time per operation.
pub fn parse(text: &str) -> Result<Vec<Timing>, String> {
    let doc: Value = serde_json::from_str(text).map_err(|e| format!("not JMH JSON: {e}"))?;
    let entries = doc.as_array().ok_or("JMH results must be a JSON array")?;
    entries.iter().map(timing).collect()
}

fn timing(entry: &Value) -> Result<Timing, String> {
    let benchmark = entry["benchmark"]
        .as_str()
        .ok_or("entry without `benchmark`")?;
    let name = match entry["params"].as_object().filter(|p| !p.is_empty()) {
        Some(params) => {
            let mut pairs: Vec<String> = params
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{k}={}",
                        v.as_str().map_or_else(|| v.to_string(), str::to_string)
                    )
                })
                .collect();
            pairs.sort();
            format!("{benchmark}[{}]", pairs.join(","))
        }
        None => benchmark.to_string(),
    };
    let primary = &entry["primaryMetric"];
    let unit = primary["scoreUnit"]
        .as_str()
        .ok_or_else(|| format!("{name}: no scoreUnit"))?;
    let raw: Vec<f64> = primary["rawData"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|fork| fork.as_array().into_iter().flatten())
        .filter_map(Value::as_f64)
        .collect();
    let to_ns = |s: f64| ns_per_op(unit, s).map_err(|e| format!("{name}: {e}"));
    if raw.is_empty() {
        return percentile_timing(name.clone(), primary, to_ns);
    }
    let samples = raw
        .into_iter()
        .map(to_ns)
        .collect::<Result<Vec<f64>, String>>()?;
    Timing::from_samples(name, samples)
}

/// Sample-time mode writes a histogram instead of `rawData`; its
/// `scorePercentiles` still give the median and the 99th percentile.
fn percentile_timing(
    name: String,
    primary: &Value,
    to_ns: impl Fn(f64) -> Result<f64, String>,
) -> Result<Timing, String> {
    let pct = |key: &str| primary["scorePercentiles"][key].as_f64();
    let median = pct("50.0")
        .or_else(|| primary["score"].as_f64())
        .ok_or_else(|| format!("{name}: no score"))?;
    Ok(Timing {
        median_ns: to_ns(median)?,
        mad_ns: 0.0,
        p99_ns: pct("99.0").map(&to_ns).transpose()?,
        name,
    })
}

/// A score in `unit` as nanoseconds per op. Throughput units are inverted.
fn ns_per_op(unit: &str, score: f64) -> Result<f64, String> {
    let time = |scale: f64| Ok(score * scale);
    let throughput = |scale: f64| {
        if score > 0.0 {
            Ok(scale / score)
        } else {
            Err(format!("a {unit} score of {score} is not a time per op"))
        }
    };
    match unit {
        "ns/op" => time(1.0),
        "us/op" => time(1e3),
        "ms/op" => time(1e6),
        "s/op" => time(1e9),
        "ops/ns" => throughput(1.0),
        "ops/us" => throughput(1e3),
        "ops/ms" => throughput(1e6),
        "ops/s" => throughput(1e9),
        other => Err(format!("unknown JMH unit {other:?}")),
    }
}
