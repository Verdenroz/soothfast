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
    pub p99_ns: Option<f64>,
    pub allocs: Option<u64>,
    pub polls: Option<u64>,
    /// Brought in by `import` from another language's harness.
    pub imported: bool,
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
            imported: m["imported"].is_string(),
        })
        .collect()
}

/// Each id of the baseline's `ambiguous` index, which has no numbers, and
/// the packages that own it.
fn ambiguous(baseline: &Value) -> Vec<(&str, String)> {
    let index = baseline["ambiguous"].as_object().into_iter().flatten();
    index
        .map(|(id, entry)| {
            let packages = entry["packages"].as_array().into_iter().flatten();
            let packages: Vec<&str> = packages.filter_map(Value::as_str).collect();
            (id.as_str(), packages.join(", "))
        })
        .collect()
}

/// The walltime columns of each imported item, grouped by import prefix.
fn by_harness(baseline: &Value) -> BTreeMap<&str, Vec<[String; 4]>> {
    let mut by_harness: BTreeMap<&str, Vec<[String; 4]>> = BTreeMap::new();
    for (id, m) in baseline["items"].as_object().into_iter().flatten() {
        if let Some(prefix) = m["imported"].as_str() {
            by_harness.entry(prefix).or_default().push([
                id.clone(),
                fmt_ns(m["walltime"]["median_ns"].as_f64()),
                fmt_ns(m["walltime"]["mad_ns"].as_f64()),
                fmt_ns(m["walltime"]["p99_ns"].as_f64()),
            ]);
        }
    }
    by_harness
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

/// `rows` rendered as a GitHub-flavored markdown table, imported harnesses
/// under `##` headings.
pub fn markdown(baseline: &Value) -> String {
    markdown_at(baseline, 2)
}

/// [`markdown`] with harness headings at `depth` `#`s, for embedding the
/// table below a heading of the same document.
pub fn markdown_at(baseline: &Value, depth: usize) -> String {
    let hashes = "#".repeat(depth);
    let mut out = String::from(
        "| item | instructions | median | p99 | allocs | polls |\n|---|---:|---:|---:|---:|---:|\n",
    );
    let rows = rows(baseline);
    let any_imported = rows.iter().any(|r| r.imported);
    for r in rows.into_iter().filter(|r| !r.imported) {
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
    for (id, packages) in ambiguous(baseline) {
        out.push_str(&format!(
            "| `{id}` (ambiguous: {packages}) | n/a | n/a | n/a | n/a | n/a |\n"
        ));
    }
    let imported = if any_imported {
        by_harness(baseline)
    } else {
        BTreeMap::new()
    };
    for (harness, rows) in imported {
        out.push_str(&format!(
            "\n{hashes} {harness}\n\nImported from the `{harness}` harness; walltime only.\n\n| item | median | mad | p99 |\n|---|---:|---:|---:|\n"
        ));
        for [id, median, mad, p99] in rows {
            out.push_str(&format!("| `{id}` | {median} | {mad} | {p99} |\n"));
        }
    }
    out
}

/// `rows` rendered as a plain `<table>` for embedding in the docs site.
pub fn html(baseline: &Value) -> String {
    let mut out = String::from(
        "<table>\n<thead><tr><th>item</th><th>instructions</th><th>median</th><th>p99</th><th>allocs</th><th>polls</th></tr></thead>\n<tbody>\n",
    );
    let rows = rows(baseline);
    let any_imported = rows.iter().any(|r| r.imported);
    for r in rows.into_iter().filter(|r| !r.imported) {
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
    for (id, packages) in ambiguous(baseline) {
        out.push_str(&format!(
            "<tr><td><code>{id}</code> (ambiguous: {packages})</td><td>n/a</td><td>n/a</td><td>n/a</td><td>n/a</td><td>n/a</td></tr>\n"
        ));
    }
    out.push_str("</tbody>\n</table>\n");
    let imported = if any_imported {
        by_harness(baseline)
    } else {
        BTreeMap::new()
    };
    for (harness, rows) in imported {
        out.push_str(&format!(
            "<h2>{harness}</h2>\n<p>Imported from the <code>{harness}</code> harness; walltime only.</p>\n<table>\n<thead><tr><th>item</th><th>median</th><th>mad</th><th>p99</th></tr></thead>\n<tbody>\n"
        ));
        for [id, median, mad, p99] in rows {
            out.push_str(&format!(
                "<tr><td><code>{id}</code></td><td>{median}</td><td>{mad}</td><td>{p99}</td></tr>\n"
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

    #[test]
    fn an_ambiguous_id_is_a_row_naming_its_packages_with_no_numbers() {
        let baseline = json!({
            "items": {},
            "ambiguous": { "util::hash": { "packages": ["a", "b"] } },
        });
        let md = super::markdown(&baseline);
        assert!(
            md.contains("| `util::hash` (ambiguous: a, b) | n/a | n/a | n/a | n/a | n/a |"),
            "{md}"
        );
        assert!(super::html(&baseline).contains("<code>util::hash</code> (ambiguous: a, b)</td>"));
    }
}
