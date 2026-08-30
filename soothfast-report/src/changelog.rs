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

/// One merged change, read off a conventional-commit subject.
pub struct Change {
    /// Commit type: `feat`, `fix`, `perf`, `docs`, and the rest.
    pub kind: String,
    /// Subject line with the type prefix and pull request number removed.
    pub subject: String,
    /// Pull request the subject ended with, when it named one.
    pub pr: Option<u32>,
}

/// Reader-facing sections, in the order a release renders them. Anything
/// whose type is missing here lands under the last entry.
const SECTIONS: [(&str, &[&str]); 5] = [
    ("Features", &["feat"]),
    ("Fixes", &["fix"]),
    ("Performance", &["perf"]),
    ("Documentation", &["docs"]),
    ("Internal", &["refactor", "chore", "test", "ci"]),
];

/// Parse `type: subject (#N)` subjects. Release and changelog bookkeeping
/// commits are dropped, since a release listing its own paperwork is noise.
pub fn changes_from_subjects(subjects: &[String]) -> Vec<Change> {
    subjects
        .iter()
        .filter_map(|line| {
            let (kind, rest) = line.split_once(": ")?;
            if !SECTIONS.iter().any(|(_, kinds)| kinds.contains(&kind)) {
                return None;
            }
            let rest = rest.trim();
            let (subject, pr) = match rest.rsplit_once(" (#") {
                Some((head, tail)) => (head, tail.strip_suffix(')').and_then(|n| n.parse().ok())),
                None => (rest, None),
            };
            let subject = subject.trim();
            let bookkeeping =
                subject.starts_with("release v") || subject.starts_with("regenerate CHANGELOG");
            (!bookkeeping).then(|| Change {
                kind: kind.to_string(),
                subject: subject.to_string(),
                pr,
            })
        })
        .collect()
}

/// Inputs already computed by the CLI (API section, two baselines).
pub struct DraftInputs<'a> {
    pub api: ApiSection<'a>,
    /// Merged changes since the reference, newest first.
    pub changes: &'a [Change],
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
    let mut out = format!("## {heading}\n\n");
    out.push_str(&sections(inputs.changes));

    let api = api_section(&inputs.api);
    let perf = perf_section(inputs);
    if api.is_empty() && perf.is_empty() {
        out.truncate(out.trim_end().len());
        out.push('\n');
        return out;
    }
    out.push_str("---\n\n");
    out.push_str(&api);
    out.push_str(&perf);
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

/// One `### <emoji> <name>` block per type that has changes, omitting the
/// rest so an empty section never ships.
fn sections(changes: &[Change]) -> String {
    let mut out = String::new();
    for (name, kinds) in SECTIONS {
        let mut entries = changes.iter().filter(|c| kinds.contains(&c.kind.as_str()));
        let Some(first) = entries.next() else {
            continue;
        };
        out.push_str(&format!("### {} {name}\n\n", section_icon(name)));
        for change in std::iter::once(first).chain(entries) {
            let subject = sentence_case(&change.subject);
            match change.pr {
                Some(pr) => out.push_str(&format!("- {subject} (#{pr})\n")),
                None => out.push_str(&format!("- {subject}\n")),
            }
        }
        out.push('\n');
    }
    out
}

/// Commit subjects are imperative and lowercase; a release note reads as a
/// list of sentences. A subject opening on an identifier is left alone,
/// since `soothfast.lock` is not a word to capitalize. `-` is deliberately
/// not in that set: `cargo-soothfast` and `version-coupled` are the same
/// shape, and the ordinary word is the commoner case.
fn sentence_case(subject: &str) -> String {
    let first_token = subject.split_whitespace().next().unwrap_or_default();
    if first_token.contains(['.', '_', '(', ':']) {
        return subject.to_string();
    }
    let mut chars = subject.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn section_icon(name: &str) -> &'static str {
    match name {
        "Features" => "\u{2728}",
        "Fixes" => "\u{1F41B}",
        "Performance" => "\u{26A1}",
        "Documentation" => "\u{1F4DD}",
        _ => "\u{1F527}",
    }
}

/// The API surface block, empty when there is nothing to report.
fn api_section(api: &ApiSection) -> String {
    let body = match api {
        ApiSection::Diff { text, .. } if is_empty_diff(text) => return String::new(),
        ApiSection::Diff { text, .. } => format!("```\n{}\n```\n", text.trim_end()),
        // An initial surface with nothing in it is a real finding, not an
        // empty section: the extraction produced nothing to describe.
        ApiSection::Initial([]) => {
            "No public items found: nothing was extracted to describe.\n".to_string()
        }
        ApiSection::Initial(entries) => inventory(entries),
    };
    format!("### \u{1F50D} API surface\n\n{body}\n")
}

/// The measured-movement table, empty when nothing moved past a threshold.
fn perf_section(inputs: &DraftInputs) -> String {
    let mut out = String::new();
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
            let mut rows = String::new();
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
                            rows.push_str(&format!(
                                "| `{id}` | {label} | {o:.1} | {n:.1} | {delta:+.1}% |\n"
                            ));
                        }
                    }
                }
            }
            if rows.is_empty() {
                return String::new();
            }
            out.push_str("| item | metric | was | now | delta |\n|---|---|---:|---:|---:|\n");
            out.push_str(&rows);
        }
    }
    format!("### \u{1F4CA} Gate movement\n\n{out}")
}

/// A diff the surface engine produced no findings for.
fn is_empty_diff(text: &str) -> bool {
    text.trim().is_empty() || text.trim() == "surface: no public API changes"
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

    use super::{ApiSection, Change, DraftInputs, PerfThresholds, changes_from_subjects, draft};
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

    fn subjects(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn drafts_diff_and_deltas() {
        let old = json!({ "items": { "demo::f": { "perfcnt": { "instructions": 100 } } } });
        let new = json!({ "items": { "demo::f": { "perfcnt": { "instructions": 90 } } } });
        let text = draft(&DraftInputs {
            changes: &[],
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

    #[test]
    fn an_unchanged_crates_sentinel_must_not_hide_findings() {
        let new = json!({ "items": {} });
        let text = draft(&DraftInputs {
            changes: &[],
            api: ApiSection::Diff {
                against: "v1.0.0",
                text: "# a\nsurface: no public API changes\n\n# b\nADDED    b::x\n",
            },
            old_baseline: None,
            new_baseline: &new,
            thresholds: gate_thresholds(),
        });
        assert!(text.contains("ADDED    b::x"), "{text}");
        assert!(!text.contains("No public API changes"), "{text}");
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
            changes: &[],
            api: ApiSection::Diff {
                against: "v1.0.0",
                text: "",
            },
            old_baseline: Some(&old),
            new_baseline: &new,
            thresholds: gate_thresholds(),
        });
        assert_eq!(text.trim(), "## Unreleased (draft vs v1.0.0)");
    }

    /// The deterministic metrics are the point: they must still land.
    #[test]
    fn an_allocation_regression_is_still_reported() {
        let old = json!({ "items": { "demo::f": {
            "walltime": { "median_ns": 100.0 }, "alloc": { "allocs": 100 } } } });
        let new = json!({ "items": { "demo::f": {
            "walltime": { "median_ns": 200.0 }, "alloc": { "allocs": 120 } } } });
        let text = draft(&DraftInputs {
            changes: &[],
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
            changes: &[],
            api: ApiSection::Diff {
                against: "v1.0.0",
                text: "",
            },
            old_baseline: None,
            new_baseline: &new,
            thresholds: gate_thresholds(),
        });
        assert!(!unchanged.contains("API surface"), "{unchanged}");
        assert!(unchanged.contains("draft vs v1.0.0"), "{unchanged}");

        let items = [entry("demo", "demo::f", "fn", "Does a thing.")];
        let initial = draft(&DraftInputs {
            changes: &[],
            api: ApiSection::Initial(&items),
            old_baseline: None,
            new_baseline: &new,
            thresholds: gate_thresholds(),
        });
        assert!(initial.contains("API surface"), "{initial}");
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
            changes: &[],
            api: ApiSection::Initial(&[]),
            old_baseline: None,
            new_baseline: &new,
            thresholds: gate_thresholds(),
        });
        assert!(text.contains("No public items found"), "{text}");
    }

    #[test]
    fn a_release_with_nothing_to_report_renders_only_its_heading() {
        let new = json!({ "items": {} });
        let old = json!({ "items": {} });
        let text = draft(&DraftInputs {
            changes: &[],
            api: ApiSection::Diff {
                against: "v1.0.0",
                text: "",
            },
            old_baseline: Some(&old),
            new_baseline: &new,
            thresholds: gate_thresholds(),
        });
        assert_eq!(text.trim(), "## Unreleased (draft vs v1.0.0)");
    }

    #[test]
    fn subjects_group_by_type_and_carry_their_pull_request() {
        let changes = changes_from_subjects(&subjects(&[
            "feat: add gate accept for reviewed regressions (#110)",
            "fix: sync untracked config into gate worktrees (#113)",
            "chore: release v0.1.19 (#112)",
            "docs: regenerate CHANGELOG.md (#115)",
            "not a conventional subject",
        ]));
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].kind, "feat");
        assert_eq!(
            changes[0].subject,
            "add gate accept for reviewed regressions"
        );
        assert_eq!(changes[0].pr, Some(110));
        assert_eq!(changes[1].pr, Some(113));
    }

    #[test]
    fn a_subject_opening_on_an_identifier_keeps_its_case() {
        let changes = changes_from_subjects(&subjects(&[
            "fix: soothfast.lock moves to v2",
            "fix: keep_alive is honored",
            "fix: resume changelog regen",
        ]));
        let rendered = super::sections(&changes);
        assert!(
            rendered.contains("- soothfast.lock moves to v2"),
            "{rendered}"
        );
        assert!(rendered.contains("- keep_alive is honored"), "{rendered}");
        assert!(rendered.contains("- Resume changelog regen"), "{rendered}");
    }

    #[test]
    fn a_subject_without_a_pull_request_still_lists() {
        let changes = changes_from_subjects(&subjects(&["fix: land a direct commit"]));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].pr, None);
    }

    #[test]
    fn only_the_types_with_changes_get_a_section() {
        let new = json!({ "items": {} });
        let changes = vec![Change {
            kind: "fix".into(),
            subject: "stop hashing comments".into(),
            pr: Some(124),
        }];
        let text = draft(&DraftInputs {
            changes: &changes,
            api: ApiSection::Diff {
                against: "v1.0.0",
                text: "",
            },
            old_baseline: None,
            new_baseline: &new,
            thresholds: gate_thresholds(),
        });
        assert!(text.contains("Fixes"), "{text}");
        assert!(text.contains("- Stop hashing comments (#124)"), "{text}");
        assert!(!text.contains("Features"), "{text}");
        assert!(!text.contains("Internal"), "{text}");
    }
}
