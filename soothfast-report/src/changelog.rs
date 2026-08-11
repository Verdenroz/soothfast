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

/// Per-metric movement a delta table is willing to report, as percentages.
///
/// These mirror the gate's own thresholds and are passed in rather than
/// defined here, so the changelog reports exactly what the gate would have
/// flagged instead of drifting from it.
///
/// Only deterministic metrics have thresholds, because a changelog is a
/// permanent record: walltime moves 15-20% between two runs of identical
/// code on a shared CI runner, so recording it means rewriting the section
/// on every merge to say nothing. Walltime regressions are `gate`'s job,
/// where a human reads the verdict against a live noise floor.
pub struct PerfThresholds {
    pub instructions_pct: f64,
    pub allocs_pct: f64,
}

/// Inputs already computed by the CLI (API section, two baselines).
pub struct DraftInputs<'a> {
    pub api: ApiSection<'a>,
    /// Reference baseline (older) — None renders current-only.
    pub old_baseline: Option<&'a Value>,
    pub new_baseline: &'a Value,
    /// Only consulted when `old_baseline` is Some.
    pub thresholds: PerfThresholds,
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
            let t = &inputs.thresholds;
            let metrics: [(&str, [&str; 2], f64); 2] = [
                (
                    "instructions",
                    ["perfcnt", "instructions"],
                    t.instructions_pct,
                ),
                ("allocs", ["alloc", "allocs"], t.allocs_pct),
            ];
            out.push_str("| item | metric | was | now | delta |\n|---|---|---:|---:|---:|\n");
            let mut any = false;
            let empty = serde_json::Map::new();
            let new_items = inputs.new_baseline["items"].as_object().unwrap_or(&empty);
            for (id, m) in new_items {
                for (label, path, threshold) in &metrics {
                    let new_v = m[path[0]][path[1]].as_f64();
                    let old_v = old["items"][id][path[0]][path[1]].as_f64();
                    if let (Some(o), Some(n)) = (old_v, new_v)
                        && o > 0.0
                    {
                        let delta = (n - o) / o * 100.0;
                        if delta.abs() >= *threshold {
                            any = true;
                            out.push_str(&format!(
                                "| `{id}` | {label} | {o:.1} | {n:.1} | {delta:+.1}% |\n"
                            ));
                        }
                    }
                }
            }
            if !any {
                out.push_str("| _no movement past gate thresholds_ | | | | |\n");
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

    use super::{ApiSection, DraftInputs, PerfThresholds, draft};
    use crate::llms::SurfaceEntry;

    /// The values `cargo-soothfast` passes from its gate constants.
    fn gate_thresholds() -> PerfThresholds {
        PerfThresholds {
            instructions_pct: 5.0,
            allocs_pct: 5.0,
        }
    }

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
            thresholds: gate_thresholds(),
        });
        assert!(text.contains("draft vs v1.0.0"));
        assert!(text.contains("ADDED    demo::g"));
        assert!(text.contains("-10.0%"));
    }

    /// A changelog is permanent, so it records only metrics that reproduce.
    /// Two CI runs of identical code moved walltime -15.2% and +18.2% on
    /// separate items, which rewrote this section on every merge.
    #[test]
    fn walltime_never_reaches_the_table_however_far_it_moved() {
        let old = json!({ "items": { "demo::f": {
            "walltime": { "median_ns": 275028.3 }, "alloc": { "allocs": 6 } } } });
        let new = json!({ "items": { "demo::f": {
            "walltime": { "median_ns": 324972.0 }, "alloc": { "allocs": 6 } } } });
        let text = draft(&DraftInputs {
            api: ApiSection::Diff {
                against: "v1.0.0",
                text: "",
            },
            old_baseline: Some(&old),
            new_baseline: &new,
            thresholds: gate_thresholds(),
        });
        assert!(text.contains("no movement past gate thresholds"), "{text}");
        assert!(!text.contains("median_ns"), "{text}");
    }

    /// The deterministic metrics are the point: they must still land.
    #[test]
    fn an_allocation_regression_is_still_reported() {
        let old = json!({ "items": { "demo::f": {
            "walltime": { "median_ns": 100.0 }, "alloc": { "allocs": 100 } } } });
        let new = json!({ "items": { "demo::f": {
            "walltime": { "median_ns": 200.0 }, "alloc": { "allocs": 120 } } } });
        let text = draft(&DraftInputs {
            api: ApiSection::Diff {
                against: "v1.0.0",
                text: "",
            },
            old_baseline: Some(&old),
            new_baseline: &new,
            thresholds: gate_thresholds(),
        });
        assert!(!text.contains("median_ns"), "{text}");
        assert!(text.contains("allocs"), "{text}");
        assert!(text.contains("+20.0%"), "{text}");
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
            thresholds: gate_thresholds(),
        });
        assert!(unchanged.contains("No public API changes."));

        let items = [entry("demo", "demo::f", "fn", "Does a thing.")];
        let initial = draft(&DraftInputs {
            api: ApiSection::Initial(&items),
            old_baseline: None,
            new_baseline: &new,
            thresholds: gate_thresholds(),
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
            thresholds: gate_thresholds(),
        });
        assert!(text.contains("No public items found"), "{text}");
    }
}
