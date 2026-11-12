//! Rendering the CI workflow init writes from the job templates.

use super::ci::Ci;
use super::ecosystem::Project;

const WORKFLOW: &str = include_str!("workflow.yml.tmpl");
const TEST_JOB: &str = include_str!("test_job.yml.tmpl");
const GATE_JOB: &str = include_str!("gate_job.yml.tmpl");
const CALLER_JOB: &str = include_str!("caller_job.yml.tmpl");
const PAGES_JOB: &str = include_str!("pages_job.yml.tmpl");
const DOCS_JOB: &str = include_str!("docs_job.yml.tmpl");
const LABELS_JOB: &str = include_str!("labels_job.yml.tmpl");

/// How often the labels job sweeps when nothing has pushed.
const SCHEDULE: &str = "  schedule:\n    - cron: \"*/30 * * * *\"\n";

pub(super) const WORKFLOW_PATH: &str = ".github/workflows/soothfast.yml";

/// What the scaffold is derived from.
pub(super) struct Workspace {
    /// Whether the root is a cargo workspace.
    pub(super) rust: bool,
    /// The non-Rust projects, in directory order.
    pub(super) projects: Vec<Project>,
    pub(super) default_branch: String,
    pub(super) ci: Ci,
    /// Packages with a `soothfast` bench target, in name order.
    pub(super) benched: Vec<String>,
    /// The packages `docs check` reads, when any markdown carries markers.
    pub(super) docs_packages: Vec<String>,
    pub(super) action: String,
    pub(super) bot: bool,
    /// Whether `[bot] labels = true` is scaffolded.
    pub(super) labels: bool,
    /// Whether `[ci] pages = true` deploys the site, which adds the job that
    /// is allowed to.
    pub(super) pages: bool,
    /// Whether the repository configures a spec, proto, coverage, capture or
    /// site check, which only the reusable workflow runs.
    pub(super) checks: bool,
    pub(super) java: String,
    pub(super) node: String,
}

impl Workspace {
    /// Whether the jobs fit in the workflow itself: a cargo workspace with
    /// nothing but tests, a gate and a docs check on the default runner.
    pub(super) fn inline(&self) -> bool {
        self.projects.is_empty() && self.ci.is_default() && !self.checks && !self.pages
    }
}

pub(super) fn workflow(ws: &Workspace) -> String {
    let mut text = WORKFLOW
        .replace("@@default_branch@@", &ws.default_branch)
        .replace("@@schedule@@", if ws.bot { SCHEDULE } else { "" });
    if ws.inline() {
        text.push_str(&cargo_jobs(ws));
        if ws.bot && ws.labels {
            text.push_str(LABELS_JOB);
        }
        text.replace("@@action@@", &ws.action)
    } else {
        text.push_str(&caller_job(ws));
        text
    }
}

fn cargo_jobs(ws: &Workspace) -> String {
    let mut text = TEST_JOB.to_string();
    if !ws.benched.is_empty() {
        text.push_str(
            &GATE_JOB
                .replace("@@packages@@", &ws.benched.join(" "))
                .replace("@@changelog@@", if ws.bot { "true" } else { "false" }),
        );
    }
    if !ws.docs_packages.is_empty() {
        text.push_str(&DOCS_JOB.replace("@@docs_package@@", &ws.docs_packages.join(" -p ")));
    }
    text
}

/// The one job of a repo with other-language projects: the reusable
/// workflow plans and runs everything from `soothfast.toml`.
fn caller_job(ws: &Workspace) -> String {
    let pin = ws.action.split_once('@').map_or("", |(_, r)| r);
    let pin = pin.split(" #").next().unwrap_or(pin);
    let inputs = [
        ("soothfast_ref", Some(format!("\"{pin}\""))),
        ("bot", ws.bot.then(|| "true".to_string())),
        (
            "runs_on",
            ws.ci
                .labels
                .as_ref()
                .map(|_| format!("'{}'", ws.ci.runs_on_json())),
        ),
        (
            "java",
            (ws.java != "21").then(|| format!("\"{}\"", ws.java)),
        ),
        (
            "node",
            (ws.node != "22").then(|| format!("\"{}\"", ws.node)),
        ),
    ];
    let with: String = inputs
        .iter()
        .filter_map(|(key, value)| Some(format!("      {key}: {}\n", value.as_ref()?)))
        .collect();
    let mut text = CALLER_JOB.replace(
        "@@action@@",
        &ws.action.replacen('@', "/.github/workflows/run.yml@", 1),
    );
    if !with.is_empty() {
        text.push_str(&format!("    with:\n{with}"));
    }
    if ws.pages {
        text.push_str(&PAGES_JOB.replace(
            "@@action@@",
            &ws.action.replacen('@', "/.github/workflows/pages.yml@", 1),
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::ecosystem::Ecosystem;
    use crate::init::lookup::{action_ref, pinned};
    use crate::init::report;
    use crate::init::scaffold::{Kind, Scaffold};
    use crate::init::test_support::{mixed_ws, project, scratch, ws};
    #[test]
    fn a_workspace_without_benches_gets_no_gate_job() {
        let text = workflow(&ws(&[], None));
        assert!(text.contains("branches: [master]"));
        assert!(text.contains("uses: Verdenroz/soothfast@v0"));
        assert!(!text.contains("gate:\n    name: Gate"));
        assert!(!text.contains("@@"));
    }

    #[test]
    fn benched_packages_and_markers_add_their_jobs() {
        let text = workflow(&ws(&["a", "b"], Some("a")));
        assert!(text.contains("packages: a b"));
        assert!(text.contains("docs check -p a"));
        assert!(!text.contains("@@"));
    }

    #[test]
    fn the_workflow_is_valid_yaml_shape() {
        let text = workflow(&ws(&["a"], Some("a")));
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            assert!(!line.starts_with('\t'), "tab indent: {line}");
        }
        assert_eq!(text.matches("\n  gate:\n").count(), 1);
        assert_eq!(text.matches("\n  docs:\n").count(), 1);
    }

    #[test]
    fn the_bot_adds_a_schedule_and_the_labels_job() {
        let plain = workflow(&ws(&[], None));
        assert!(!plain.contains("schedule:") && !plain.contains("labels: \"true\""));
        let bot = workflow(&Workspace {
            bot: true,
            labels: true,
            ..ws(&["a"], None)
        });
        assert!(bot.contains("  schedule:\n    - cron:"));
        assert!(bot.contains("\n  labels:\n"));
        assert!(bot.contains("changelog: \"true\""));
        assert!(!bot.contains("@@"));
    }

    #[test]
    fn only_the_labels_job_sweeps() {
        let text = workflow(&Workspace {
            bot: true,
            labels: true,
            docs_packages: vec!["a".into()],
            ..ws(&["a"], None)
        });
        assert_eq!(text.matches("labels: \"false\"").count(), 3);
        assert_eq!(text.matches("labels: \"true\"").count(), 1);
    }

    #[test]
    fn a_bot_without_labels_adds_no_labels_job() {
        let w = Workspace {
            bot: true,
            labels: false,
            ..ws(&["a"], None)
        };
        assert!(!workflow(&w).contains("\n  labels:\n"));
    }

    #[test]
    fn projects_render_one_caller_job_of_the_reusable_workflow() {
        let sha = "c".repeat(40);
        let w = Workspace {
            action: format!("Verdenroz/soothfast@{sha} # v1.2.3"),
            ..mixed_ws(true, vec![project(Ecosystem::Gradle, "client")])
        };
        let text = workflow(&w);
        let reusable =
            format!("uses: Verdenroz/soothfast/.github/workflows/run.yml@{sha} # v1.2.3");
        assert!(text.contains(&reusable), "{text}");
        assert!(!text.contains("secrets"));
        assert_eq!(text.matches("\n  soothfast:\n").count(), 1);
        assert_eq!(text.matches("    uses: ").count(), 1);
        assert!(!text.contains("@@"));
        assert!(text.contains(&format!("    with:\n      soothfast_ref: \"{sha}\"\n")));
        let stale = text.replace(
            &format!("soothfast_ref: \"{sha}\""),
            "soothfast_ref: \"old\"",
        );
        let root = scratch("ref");
        report(
            &root,
            &[Scaffold {
                path: WORKFLOW_PATH.into(),
                kind: Kind::Whole,
                content: text.clone(),
            }],
            false,
            false,
        );
        let moved = [Scaffold {
            path: WORKFLOW_PATH.into(),
            kind: Kind::Whole,
            content: stale,
        }];
        assert_eq!(report(&root, &moved, true, false), 1);
        assert!(text.contains("branches: [master]"));
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            assert!(!line.starts_with('\t'), "tab indent: {line}");
        }
    }

    #[test]
    fn a_cargo_workspace_with_checks_or_pages_calls_the_reusable_workflow() {
        let plain = ws(&["a"], None);
        assert!(!workflow(&plain).contains("/.github/workflows/run.yml@"));
        for w in [
            Workspace {
                checks: true,
                ..ws(&["a"], None)
            },
            Workspace {
                pages: true,
                ..ws(&["a"], None)
            },
        ] {
            assert!(workflow(&w).contains("/.github/workflows/run.yml@"));
        }
    }

    #[test]
    fn the_caller_may_deploy_pages_only_when_the_file_asks() {
        let on = Workspace {
            pages: true,
            ..mixed_ws(false, vec![project(Ecosystem::Node, ".")])
        };
        let text = workflow(&on);
        assert!(text.contains("  pages:\n    needs: soothfast\n"), "{text}");
        assert!(text.contains("      pages: write\n"));
        assert!(text.contains("/.github/workflows/pages.yml@v0\n"), "{text}");
        let off = workflow(&mixed_ws(false, vec![project(Ecosystem::Node, ".")]));
        assert!(!off.contains("pages"), "{off}");
    }

    #[test]
    fn the_caller_passes_the_bot_and_toolchain_inputs_that_differ_from_the_defaults() {
        let w = Workspace {
            bot: true,
            node: "20".into(),
            ..mixed_ws(false, vec![project(Ecosystem::Node, ".")])
        };
        let text = workflow(&w);
        assert!(
            text.contains("      soothfast_ref: \"v0\"\n      bot: true\n      node: \"20\"\n")
        );
        assert!(text.contains("  schedule:\n    - cron:"));
        assert!(!text.contains("labels:") && !text.contains("java:"));
        assert_eq!(pinned(&text, "node").as_deref(), Some("20"));
        assert_eq!(pinned(&text, "java"), None);
    }

    #[test]
    fn a_rust_only_repo_keeps_its_own_jobs() {
        let text = workflow(&ws(&["a"], Some("a")));
        assert!(!text.contains("run.yml") && !text.contains("secrets: inherit"));
        assert!(text.contains("\n  test:\n") && text.contains("\n  gate:\n"));
        let bot = workflow(&Workspace {
            bot: true,
            labels: true,
            ..ws(&[], None)
        });
        assert!(bot.contains("\n  labels:\n") && !bot.contains("version:"));
    }

    #[test]
    fn the_caller_pin_survives_a_flagless_check() {
        let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
        let action = format!("Verdenroz/soothfast@{} # {tag}", "d".repeat(40));
        let w = Workspace {
            action: action.clone(),
            ..mixed_ws(false, vec![project(Ecosystem::Python, ".")])
        };
        assert_eq!(action_ref(&workflow(&w), None, true), action);
    }

    #[test]
    fn the_toolchain_versions_survive_a_flagless_check() {
        let mut w = mixed_ws(false, vec![project(Ecosystem::Node, ".")]);
        w.node = "20".into();
        let text = workflow(&w);
        assert_eq!(pinned(&text, "node").as_deref(), Some("20"));
        assert_eq!(pinned(&text, "java"), None);
    }

    #[test]
    fn a_ci_table_moves_a_rust_only_repo_to_the_reusable_workflow() {
        let w = Workspace {
            ci: Ci::parse("[runner]\nlabels = [\"self-hosted\", \"linux\"]\n").unwrap(),
            ..ws(&["a"], None)
        };
        let text = workflow(&w);
        assert!(text.contains("run.yml@"), "{text}");
        assert!(text.contains("      runs_on: '[\"self-hosted\",\"linux\"]'\n"));
        assert!(!text.contains("\n  test:\n") && !text.contains("@@"));
    }
}
