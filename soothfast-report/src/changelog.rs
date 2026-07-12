//! Changelog drafting: API-surface diff + perf deltas rendered as a draft
//! section, assembled from recorded facts instead of backfilled from memory.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::llms::SurfaceEntry;

/// What the API section has to report.
///
/// The two cases are separate types rather than an empty-string convention:
/// "nothing changed" and "there was nothing to compare against" read the
/// same in prose but mean opposite things, and a changelog that confuses
/// them reports calm where it had no input at all.
pub enum ApiSection<'a> {
    /// Diff against an earlier release.
    Diff { against: &'a str, text: &'a str },
    /// No earlier release exists, so the surface itself is the news.
    Initial(&'a [SurfaceEntry]),
}

/// Inputs already computed by the CLI (API section, two baselines).
pub struct DraftInputs<'a> {
    pub api: ApiSection<'a>,
    /// Reference baseline (older) — None renders current-only.
    pub old_baseline: Option<&'a Value>,
    pub new_baseline: &'a Value,
}

/// Render the "Unreleased" draft section: API surface + perf table.
pub fn draft(inputs: &DraftInputs) -> String {
    let heading = match &inputs.api {
        ApiSection::Diff { against, .. } => format!("Unreleased (draft vs {against})"),
        ApiSection::Initial(_) => "Unreleased (initial public surface)".to_string(),
    };
    let mut out = format!("## {heading}\n\n### API surface\n\n");
    match &inputs.api {
        ApiSection::Diff { text, .. } if is_empty_diff(text) => {
            out.push_str("No public API changes.\n");
        }
        ApiSection::Diff { text, .. } => {
            out.push_str("```\n");
            out.push_str(text.trim_end());
            out.push_str("\n```\n");
        }
        ApiSection::Initial([]) => {
            out.push_str("No public items found: nothing was extracted to describe.\n");
        }
        ApiSection::Initial(entries) => out.push_str(&inventory(entries)),
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

/// A diff the surface engine produced no findings for.
fn is_empty_diff(text: &str) -> bool {
    text.trim().is_empty() || text.contains("no public API changes")
}

/// Every public item, grouped by crate. Nothing is capped: a first release
/// listing "the 12 most interesting items" would be a claim about taste,
/// not a record of what shipped.
fn inventory(entries: &[SurfaceEntry]) -> String {
    let mut by_pkg: BTreeMap<&str, Vec<&SurfaceEntry>> = BTreeMap::new();
    for e in entries {
        by_pkg.entry(&e.pkg).or_default().push(e);
    }

    let mut out = format!(
        "Initial release: {} public items across {} crates.\n",
        entries.len(),
        by_pkg.len()
    );
    for (pkg, mut items) in by_pkg {
        items.sort_by(|a, b| a.path.cmp(&b.path));
        out.push_str(&format!("\n#### `{pkg}` ({})\n\n", items.len()));
        for e in items {
            out.push_str(&format!("- `{}` ({})", e.path, e.kind));
            if !e.summary.trim().is_empty() {
                out.push_str(&format!(": {}", e.summary.trim()));
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ApiSection, DraftInputs, draft};
    use crate::llms::SurfaceEntry;

    fn entry(pkg: &str, path: &str, kind: &str, summary: &str) -> SurfaceEntry {
        SurfaceEntry {
            path: path.into(),
            kind: kind.into(),
            signature: String::new(),
            pkg: pkg.into(),
            summary: summary.into(),
            docs: String::new(),
        }
    }

    #[test]
    fn drafts_diff_and_deltas() {
        let old = json!({ "items": { "demo::f": { "perfcnt": { "instructions": 100 } } } });
        let new = json!({ "items": { "demo::f": { "perfcnt": { "instructions": 90 } } } });
        let text = draft(&DraftInputs {
            api: ApiSection::Diff {
                against: "v1.0.0",
                text: "ADDED    demo::g\n",
            },
            old_baseline: Some(&old),
            new_baseline: &new,
        });
        assert!(text.contains("draft vs v1.0.0"));
        assert!(text.contains("ADDED    demo::g"));
        assert!(text.contains("-10.0%"));
    }

    /// The bug this section's types exist to prevent: an empty diff and a
    /// missing reference are different facts and must not read alike.
    #[test]
    fn nothing_to_compare_against_does_not_claim_nothing_changed() {
        let new = json!({ "items": {} });
        let unchanged = draft(&DraftInputs {
            api: ApiSection::Diff {
                against: "v1.0.0",
                text: "",
            },
            old_baseline: None,
            new_baseline: &new,
        });
        assert!(unchanged.contains("No public API changes."));

        let items = [entry("demo", "demo::f", "fn", "Does a thing.")];
        let initial = draft(&DraftInputs {
            api: ApiSection::Initial(&items),
            old_baseline: None,
            new_baseline: &new,
        });
        assert!(!initial.contains("No public API changes."), "{initial}");
        assert!(initial.contains("initial public surface"), "{initial}");
        assert!(
            initial.contains("1 public items across 1 crates"),
            "{initial}"
        );
        assert!(
            initial.contains("`demo::f` (fn): Does a thing."),
            "{initial}"
        );
    }

    #[test]
    fn an_empty_initial_surface_says_so_rather_than_looking_calm() {
        let new = json!({ "items": {} });
        let text = draft(&DraftInputs {
            api: ApiSection::Initial(&[]),
            old_baseline: None,
            new_baseline: &new,
        });
        assert!(text.contains("No public items found"), "{text}");
    }
}
