//! Perf tables from a baseline document: one row per item, the deterministic
//! metrics first.

use serde_json::Value;

/// One measured item's metrics, each `None` when that backend didn't run.
pub struct Row {
    pub id: String,
    pub instructions: Option<u64>,
    pub median_ns: Option<f64>,
    pub p99_ns: Option<f64>,
    pub allocs: Option<u64>,
    pub polls: Option<u64>,
}

/// One `Row` per measured item in the baseline, in baseline (map) order.
pub fn rows(baseline: &Value) -> Vec<Row> {
    let Some(items) = baseline["items"].as_object() else {
        return Vec::new();
    };
    items
        .iter()
        .map(|(id, m)| Row {
            id: id.clone(),
            instructions: m["perfcnt"]["instructions"].as_u64(),
            median_ns: m["walltime"]["median_ns"].as_f64(),
            p99_ns: m["walltime"]["p99_ns"].as_f64(),
            allocs: m["alloc"]["allocs"].as_u64(),
            polls: m["asyncexec"]["polls"].as_u64(),
        })
        .collect()
}

fn fmt_u(v: Option<u64>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "—".into())
}
fn fmt_ns(v: Option<f64>) -> String {
    match v {
        Some(ns) if ns >= 1e6 => format!("{:.2}ms", ns / 1e6),
        Some(ns) if ns >= 1e3 => format!("{:.2}µs", ns / 1e3),
        Some(ns) => format!("{ns:.1}ns"),
        None => "—".into(),
    }
}

/// `rows` rendered as a GitHub-flavored markdown table.
pub fn markdown(baseline: &Value) -> String {
    let mut out = String::from(
        "| item | instructions | median | p99 | allocs | polls |\n|---|---:|---:|---:|---:|---:|\n",
    );
    for r in rows(baseline) {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} |\n",
            r.id,
            fmt_u(r.instructions),
            fmt_ns(r.median_ns),
            fmt_ns(r.p99_ns),
            fmt_u(r.allocs),
            fmt_u(r.polls),
        ));
    }
    out
}

/// `rows` rendered as a plain `<table>` for embedding in the docs site.
pub fn html(baseline: &Value) -> String {
    let mut out = String::from(
        "<table>\n<thead><tr><th>item</th><th>instructions</th><th>median</th><th>p99</th><th>allocs</th><th>polls</th></tr></thead>\n<tbody>\n",
    );
    for r in rows(baseline) {
        out.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            r.id,
            fmt_u(r.instructions),
            fmt_ns(r.median_ns),
            fmt_ns(r.p99_ns),
            fmt_u(r.allocs),
            fmt_u(r.polls),
        ));
    }
    out.push_str("</tbody>\n</table>\n");
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn renders_markdown_rows() {
        let baseline = json!({ "items": { "demo::f": {
            "perfcnt": { "instructions": 98 },
            "walltime": { "median_ns": 13.7, "p99_ns": 15.0 },
            "alloc": { "allocs": 0, "bytes": 0 }
        }}});
        let md = super::markdown(&baseline);
        assert!(md.contains("| `demo::f` | 98 | 13.7ns | 15.0ns | 0 | — |"));
    }
}
