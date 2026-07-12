//! Changelog drafting: API-surface diff + perf deltas rendered as a draft
//! section, assembled from recorded facts instead of backfilled from memory.

use serde_json::Value;

/// Inputs already computed by the CLI (surface diff text, two baselines).
pub struct DraftInputs<'a> {
    pub against: &'a str,
    pub surface_diff: &'a str,
    /// Reference baseline (older) — None renders current-only.
    pub old_baseline: Option<&'a Value>,
    pub new_baseline: &'a Value,
}

/// Render the "Unreleased" draft section: API surface diff + perf table.
pub fn draft(inputs: &DraftInputs) -> String {
    let mut out = format!(
        "## Unreleased (draft vs {})\n\n### API surface\n\n",
        inputs.against
    );
    if inputs.surface_diff.trim().is_empty()
        || inputs.surface_diff.contains("no public API changes")
    {
        out.push_str("No public API changes.\n");
    } else {
        out.push_str("```\n");
        out.push_str(inputs.surface_diff.trim_end());
        out.push_str("\n```\n");
    }

    out.push_str("\n### Performance\n\n");
    match inputs.old_baseline {
        None => {
            out.push_str(&crate::perf_table::markdown(inputs.new_baseline));
        }
        Some(old) => {
            out.push_str("| item | metric | was | now | delta |\n|---|---|---:|---:|---:|\n");
            let mut any = false;
            let empty = serde_json::Map::new();
            let new_items = inputs.new_baseline["items"].as_object().unwrap_or(&empty);
            for (id, m) in new_items {
                for (label, path) in [
                    ("instructions", ["perfcnt", "instructions"]),
                    ("median_ns", ["walltime", "median_ns"]),
                    ("allocs", ["alloc", "allocs"]),
                ] {
                    let new_v = m[path[0]][path[1]].as_f64();
                    let old_v = old["items"][id][path[0]][path[1]].as_f64();
                    if let (Some(o), Some(n)) = (old_v, new_v)
                        && o > 0.0
                    {
                        let delta = (n - o) / o * 100.0;
                        if delta.abs() >= 1.0 {
                            any = true;
                            out.push_str(&format!(
                                "| `{id}` | {label} | {o:.1} | {n:.1} | {delta:+.1}% |\n"
                            ));
                        }
                    }
                }
            }
            if !any {
                out.push_str("| _no per-item deltas ≥ 1%_ | | | | |\n");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn drafts_diff_and_deltas() {
        let old = json!({ "items": { "demo::f": { "perfcnt": { "instructions": 100 } } } });
        let new = json!({ "items": { "demo::f": { "perfcnt": { "instructions": 90 } } } });
        let text = super::draft(&super::DraftInputs {
            against: "v1.0.0",
            surface_diff: "ADDED    demo::g\n",
            old_baseline: Some(&old),
            new_baseline: &new,
        });
        assert!(text.contains("ADDED    demo::g"));
        assert!(text.contains("-10.0%"));
    }
}
