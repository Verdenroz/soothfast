//! `vitest bench --outputJson` output.

use serde_json::Value;

use super::Timing;

/// Parse a vitest bench result, ids named `<group full name> > <benchmark>`
/// as vitest prints them. Vitest reports milliseconds and summary stats only,
/// so there are no samples: the MAD is `mad` when present, else
/// `sd * 0.6745`, which holds for roughly normal timings only.
pub fn parse(text: &str) -> Result<Vec<Timing>, String> {
    let doc: Value =
        serde_json::from_str(text).map_err(|e| format!("not vitest bench JSON: {e}"))?;
    let files = doc["files"]
        .as_array()
        .ok_or("vitest results need a `files` array")?;
    let mut timings = Vec::new();
    for group in files
        .iter()
        .flat_map(|f| f["groups"].as_array().into_iter().flatten())
    {
        let group_name = group["fullName"]
            .as_str()
            .ok_or("vitest group without `fullName`")?;
        for bench in group["benchmarks"].as_array().into_iter().flatten() {
            timings.push(timing(group_name, bench)?);
        }
    }
    Ok(timings)
}

fn timing(group: &str, bench: &Value) -> Result<Timing, String> {
    let name = format!(
        "{group} > {}",
        bench["name"]
            .as_str()
            .ok_or("vitest benchmark without `name`")?
    );
    let ns = |key: &str| bench[key].as_f64().map(|ms| ms * 1e6);
    let median_ns = ns("median").ok_or_else(|| format!("{name}: no median"))?;
    let mad_ns = ns("mad")
        .or_else(|| ns("sd").map(|sd| sd * 0.6745))
        .ok_or_else(|| format!("{name}: no mad or sd"))?;
    Ok(Timing {
        median_ns,
        mad_ns,
        p99_ns: ns("p99"),
        name,
    })
}
