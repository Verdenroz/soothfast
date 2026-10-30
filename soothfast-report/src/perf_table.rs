//! Perf tables from a baseline document: one row per item, the deterministic
//! metrics first. Items an `import` brought in from another language's
//! harness get a walltime-only section per import prefix.

use std::collections::BTreeMap;

use serde_json::Value;

/// One measured item's metrics, each `None` when that backend didn't run.
pub struct Row {
    pub id: String,
    pub instructions: Option<u64>,
    pub median_ns: Option<f64>,
    pub mad_ns: Option<f64>,
    pub p99_ns: Option<f64>,
    pub allocs: Option<u64>,
    pub polls: Option<u64>,
    /// Import prefix (the harness) when the item was imported.
    pub imported: Option<String>,
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
            mad_ns: m["walltime"]["mad_ns"].as_f64(),
            p99_ns: m["walltime"]["p99_ns"].as_f64(),
            allocs: m["alloc"]["allocs"].as_u64(),
            polls: m["asyncexec"]["polls"].as_u64(),
            imported: m["imported"].as_str().map(String::from),
        })
        .collect()
}

/// Rust items (no import prefix) and, per import prefix, the imported ones.
fn split(baseline: &Value) -> (Vec<Row>, BTreeMap<String, Vec<Row>>) {
    let (imported, native): (Vec<Row>, Vec<Row>) = rows(baseline)
        .into_iter()
        .partition(|r| r.imported.is_some());
    let mut by_harness: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    for r in imported {
        by_harness
            .entry(r.imported.clone().unwrap_or_default())
            .or_default()
            .push(r);
    }
    (native, by_harness)
}

fn fmt_u(v: Option<u64>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "n/a".into())
}
fn fmt_ns(v: Option<f64>) -> String {
    match v {
        Some(ns) if ns >= 1e6 => format!("{:.2}ms", ns / 1e6),
        Some(ns) if ns >= 1e3 => format!("{:.2}µs", ns / 1e3),
        Some(ns) => format!("{ns:.1}ns"),
        None => "n/a".into(),
    }
}

/// `rows` rendered as a GitHub-flavored markdown table.
pub fn markdown(baseline: &Value) -> String {
    let mut out = String::from(
        "| item | instructions | median | p99 | allocs | polls |\n|---|---:|---:|---:|---:|---:|\n",
    );
    let (native, imported) = split(baseline);
    for r in native {
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
    for (harness, rows) in imported {
        out.push_str(&format!(
            "\n## {harness}\n\nImported from the `{harness}` harness; walltime only.\n\n| item | median | mad | p99 |\n|---|---:|---:|---:|\n"
        ));
        for r in rows {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                r.id,
                fmt_ns(r.median_ns),
                fmt_ns(r.mad_ns),
                fmt_ns(r.p99_ns),
            ));
        }
    }
    out
}

/// `rows` rendered as a plain `<table>` for embedding in the docs site.
pub fn html(baseline: &Value) -> String {
    let mut out = String::from(
        "<table>\n<thead><tr><th>item</th><th>instructions</th><th>median</th><th>p99</th><th>allocs</th><th>polls</th></tr></thead>\n<tbody>\n",
    );
    let (native, imported) = split(baseline);
    for r in native {
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
    for (harness, rows) in imported {
        out.push_str(&format!(
            "<h2>{harness}</h2>\n<p>Imported from the <code>{harness}</code> harness; walltime only.</p>\n<table>\n<thead><tr><th>item</th><th>median</th><th>mad</th><th>p99</th></tr></thead>\n<tbody>\n"
        ));
        for r in rows {
            out.push_str(&format!(
                "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                r.id,
                fmt_ns(r.median_ns),
                fmt_ns(r.mad_ns),
                fmt_ns(r.p99_ns),
            ));
        }
        out.push_str("</tbody>\n</table>\n");
    }
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
        assert!(md.contains("| `demo::f` | 98 | 13.7ns | 15.0ns | 0 | n/a |"));
    }

    #[test]
    fn imported_items_get_a_section_per_harness() {
        let baseline = json!({ "items": {
            "demo::f": { "walltime": { "median_ns": 13.7, "p99_ns": 15.0 } },
            "jmh::A.b": { "imported": "jmh",
                "walltime": { "median_ns": 2500.0, "mad_ns": 40.0, "p99_ns": 3100.0 } },
            "pytest::t.py::test_x": { "imported": "pytest",
                "walltime": { "median_ns": 900.0, "mad_ns": 5.0, "p99_ns": 950.0 } }
        }});
        let md = super::markdown(&baseline);
        let (native, rest) = md.split_once("\n## jmh\n").unwrap();
        assert!(native.contains("`demo::f`") && !native.contains("jmh::A.b"));
        assert!(rest.contains("| `jmh::A.b` | 2.50µs | 40.0ns | 3.10µs |"));
        assert!(rest.contains("\n## pytest\n"));
        assert!(super::html(&baseline).contains("<h2>jmh</h2>"));
    }
}
