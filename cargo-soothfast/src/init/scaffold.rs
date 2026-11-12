//! The files init scaffolds and how each is held to what init writes.

use std::path::{Path, PathBuf};

use super::bot::{bot_enabled, has_bot_table};
use super::ci::Ci;
use super::ecosystem::{self, parse_projects};
use super::textdiff;
use super::workflow::{WORKFLOW_PATH, Workspace, workflow};

const SOOTHFAST_TOML: &str = "\
# Counters (callgrind or perf) gate; walltime is reported and never fails,
# which halves the merge-base rounds a gate measures.
[gate]
walltime = \"advisory\"
";

const IMPORT_TOML: &str = "\
# Imported gates measure walltime only, both sides on the same runner.
# Set it to \"advisory\" if the runner is too noisy to trust.
[gate]
walltime = \"hard\"
";

/// How a scaffolded file is held to what init writes.
#[derive(Debug, PartialEq)]
pub(super) enum Kind {
    /// Exactly this content; any difference is drift.
    Whole,
    /// Written once when absent, then the repo's own.
    Seed,
    /// One line the file must contain.
    Line,
    /// The `[[project]]` tables, replaced when their settings change.
    Section,
    /// The `[runner]` table, replaced when its labels change.
    Runner,
    /// `[bot] labels = true`: the table is appended when absent and
    /// never rewritten once the repo has one.
    Bot,
    /// The `ci` key of the `[test]` table, set in place.
    TestCi,
    /// The `package` key of the `[docs]` table, set in place.
    Docs,
}

impl Kind {
    /// The tables a rewrite replaces.
    fn tables(&self) -> &'static [&'static str] {
        match self {
            Kind::Runner => &["runner"],
            _ => &[
                "init",
                ecosystem::PROJECT_TABLE,
                ecosystem::LEGACY_PROJECT_TABLE,
            ],
        }
    }
}

/// Whether two `.gitignore` lines name the same root entry, so `/.soothfast`
/// and `.soothfast/` count as one.
fn same_ignore(a: &str, b: &str) -> bool {
    let bare = |l: &str| l.trim().trim_matches('/').to_string();
    bare(a) == bare(b)
}

#[derive(Debug)]
pub(super) struct Scaffold {
    pub(super) path: PathBuf,
    pub(super) kind: Kind,
    pub(super) content: String,
}

#[derive(Debug, PartialEq)]
pub(super) enum State {
    Current,
    Missing,
    Differs,
}

impl Scaffold {
    pub(super) fn state(&self, text: Option<&str>) -> State {
        let Some(text) = text else {
            return State::Missing;
        };
        match self.kind {
            Kind::Seed => State::Current,
            Kind::Whole if text == self.content => State::Current,
            Kind::Whole => State::Differs,
            Kind::Line if text.lines().any(|l| same_ignore(l, &self.content)) => State::Current,
            Kind::Line => State::Differs,
            Kind::Section if parse_projects(text) == parse_projects(&self.content) => {
                State::Current
            }
            Kind::Section => State::Differs,
            Kind::Bot if bot_enabled(text) => State::Current,
            Kind::Runner if labels(text) == labels(&self.content) => State::Current,
            Kind::TestCi if test_ci(text) == test_ci(&self.content) => State::Current,
            Kind::Docs if docs(text) == docs(&self.content) => State::Current,
            Kind::Runner | Kind::TestCi | Kind::Docs | Kind::Bot => State::Differs,
        }
    }

    /// Whether the file still saves projects as an `[init]` or
    /// `[[init.project]]` table.
    pub(super) fn holds_legacy(&self, text: &str) -> bool {
        self.kind == Kind::Section
            && text.lines().any(|l| {
                matches!(
                    ecosystem::table_name(l),
                    Some("init" | ecosystem::LEGACY_PROJECT_TABLE)
                )
            })
    }

    /// The file as `init` leaves it, given what it holds now.
    pub(super) fn updated(&self, current: &str) -> String {
        match self.kind {
            Kind::Line => {
                let mut text = current.to_string();
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&self.content);
                text.push('\n');
                text
            }
            Kind::TestCi => with_key(current, "test", "ci", "ci = false"),
            Kind::Docs => {
                let line = self
                    .content
                    .lines()
                    .find(|l| l.starts_with("package"))
                    .unwrap_or_default();
                with_key(current, "docs", "package", line)
            }
            Kind::Bot if has_bot_table(current) => current.to_string(),
            Kind::Bot => {
                let mut text = current.trim_end().to_string();
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(&self.content);
                text
            }
            Kind::Section | Kind::Runner => {
                let mut text = without_tables(current, self.kind.tables());
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&self.content);
                text
            }
            Kind::Whole | Kind::Seed => self.content.clone(),
        }
    }

    /// Write the file as `init` leaves it, given what it holds now, and
    /// return that text.
    pub(super) fn write(&self, root: &Path, current: &str) -> std::io::Result<String> {
        let path = root.join(&self.path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = self.updated(current);
        std::fs::write(path, &text)?;
        Ok(text)
    }
}

/// What the file at `path` holds, `None` when it does not exist. Any other
/// failure, such as text that is not UTF-8, is an error: the file is there
/// and must not be taken for an empty one.
pub(super) fn read_text(root: &Path, path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(root.join(path)) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// What `init` would change in one file, as a unified diff: every scaffold
/// that targets it, applied in turn.
pub(super) fn path_diff(scaffolds: &[&Scaffold], current: &str) -> String {
    let mut text = current.to_string();
    for f in scaffolds {
        if f.state(Some(current)) != State::Current {
            text = f.updated(&text);
        }
    }
    let by = if scaffolds.iter().any(|f| f.kind == Kind::Whole) {
        "init --force"
    } else {
        "init"
    };
    let path = scaffolds[0].path.display();
    let hunks = textdiff::unified(current, &text);
    format!("--- {path} (current)\n+++ {path} ({by} would write)\n{hunks}")
}

fn labels(text: &str) -> Result<Option<Vec<String>>, String> {
    Ci::parse(text).map(|ci| ci.labels)
}

fn test_ci(text: &str) -> Result<bool, String> {
    Ci::parse(text).map(|ci| ci.runs_test())
}

fn docs(text: &str) -> Result<Option<Vec<String>>, String> {
    Ci::parse(text).map(|ci| ci.docs)
}

/// `text` with `entry` as the `key` of `table`, which is added when absent.
fn with_key(text: &str, table: &str, key: &str, entry: &str) -> String {
    let mut out = String::new();
    let mut in_table = false;
    let mut found = false;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with('[') {
            in_table = ecosystem::table_name(line) == Some(table);
            found |= in_table;
            out.push_str(line);
            if in_table {
                out.push_str(&format!("{entry}\n"));
            }
        } else if !(in_table && line.split_once('=').is_some_and(|(k, _)| k.trim() == key)) {
            out.push_str(line);
        }
    }
    if !found {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("[{table}]\n{entry}\n"));
    }
    out
}

/// `text` without the `names` tables, trimmed to end in one newline.
fn without_tables(text: &str, names: &[&str]) -> String {
    let mut skipping = false;
    let mut kept = String::new();
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with('[') {
            skipping = ecosystem::table_name(line).is_some_and(|name| names.contains(&name));
        }
        if !skipping {
            kept.push_str(line);
        }
    }
    let kept = kept.trim_end();
    if kept.is_empty() {
        String::new()
    } else {
        format!("{kept}\n")
    }
}

pub(super) fn scaffold(ws: &Workspace) -> Vec<Scaffold> {
    let mut files = vec![
        Scaffold {
            path: "soothfast.toml".into(),
            kind: Kind::Seed,
            content: if ws.rust { SOOTHFAST_TOML } else { IMPORT_TOML }.into(),
        },
        Scaffold {
            path: ".gitignore".into(),
            kind: Kind::Line,
            content: ".soothfast/".into(),
        },
    ];
    files.insert(
        0,
        Scaffold {
            path: WORKFLOW_PATH.into(),
            kind: Kind::Whole,
            content: workflow(ws),
        },
    );
    if !ws.projects.is_empty() {
        files.push(Scaffold {
            path: "soothfast.toml".into(),
            kind: Kind::Section,
            content: ecosystem::section(&ws.projects),
        });
    }
    if ws.ci.labels.is_some() {
        files.push(Scaffold {
            path: "soothfast.toml".into(),
            kind: Kind::Runner,
            content: ws.ci.runner_section(),
        });
    }
    if ws.labels {
        files.push(Scaffold {
            path: "soothfast.toml".into(),
            kind: Kind::Bot,
            content: "[bot]\nlabels = true\n".into(),
        });
    }
    if !ws.docs_packages.is_empty() {
        files.push(Scaffold {
            path: "soothfast.toml".into(),
            kind: Kind::Docs,
            content: format!("[docs]\n{}\n", Ci::docs_line(&ws.docs_packages)),
        });
    }
    if !ws.ci.runs_test() {
        files.push(Scaffold {
            path: "soothfast.toml".into(),
            kind: Kind::TestCi,
            content: "[test]\nci = false\n".into(),
        });
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::report;
    use crate::init::test_support::{scratch, ws};
    #[test]
    fn no_labels_flag_scaffolds_no_bot_table() {
        let files = scaffold(&ws(&[], None));
        assert!(files.iter().all(|f| f.kind != Kind::Bot));
    }

    #[test]
    fn a_seed_is_written_once_and_a_line_is_appended_once() {
        let root = scratch("seed");
        let seed = Scaffold {
            path: "s.toml".into(),
            kind: Kind::Seed,
            content: "x".into(),
        };
        let line = Scaffold {
            path: ".gitignore".into(),
            kind: Kind::Line,
            content: ".soothfast/".into(),
        };
        std::fs::write(root.join(".gitignore"), "target").unwrap();
        let held = |f: &Scaffold| read_text(&root, &f.path).unwrap();
        assert_eq!(seed.state(held(&seed).as_deref()), State::Missing);
        assert_eq!(line.state(held(&line).as_deref()), State::Differs);
        seed.write(&root, "").unwrap();
        line.write(&root, "target").unwrap();
        std::fs::write(root.join("s.toml"), "edited").unwrap();
        assert_eq!(seed.state(held(&seed).as_deref()), State::Current);
        assert_eq!(line.state(held(&line).as_deref()), State::Current);
        assert_eq!(
            std::fs::read_to_string(root.join(".gitignore")).unwrap(),
            "target\n.soothfast/\n"
        );
    }

    #[test]
    fn any_root_form_of_the_ignore_line_counts_as_present() {
        let line = Scaffold {
            path: ".gitignore".into(),
            kind: Kind::Line,
            content: ".soothfast/".into(),
        };
        for form in [".soothfast", ".soothfast/", "/.soothfast", "/.soothfast/"] {
            assert_eq!(
                line.state(Some(&format!("target\n{form}\n"))),
                State::Current
            );
        }
        assert_eq!(line.state(Some("target\n")), State::Differs);
        assert_eq!(line.state(Some("sub/.soothfast/\n")), State::Differs);
    }

    fn whole(path: &str, content: &str) -> Scaffold {
        Scaffold {
            path: path.into(),
            kind: Kind::Whole,
            content: content.into(),
        }
    }

    #[test]
    fn a_drifted_pin_shows_as_a_changed_line() {
        let root = scratch("diff-pin");
        let workflow = whole(
            "ci.yml",
            "jobs:\n  gate:\n    uses: acme/soothfast@v1\n    with: {}\n",
        );
        let held = "jobs:\n  gate:\n    uses: acme/soothfast@v0\n    with: {}\n";
        std::fs::write(root.join("ci.yml"), held).unwrap();
        assert_eq!(
            path_diff(&[&workflow], held),
            "--- ci.yml (current)\n+++ ci.yml (init --force would write)\n\
             @@ -1,4 +1,4 @@\n jobs:\n   gate:\n-    uses: acme/soothfast@v0\n\
             +    uses: acme/soothfast@v1\n     with: {}\n"
        );
        assert_eq!(report(&root, &[workflow], true, false), 1);
    }

    #[test]
    fn a_missing_ignore_line_shows_as_an_added_line() {
        let ignore = Scaffold {
            path: ".gitignore".into(),
            kind: Kind::Line,
            content: ".soothfast/".into(),
        };
        assert_eq!(
            path_diff(&[&ignore], "target\n"),
            "--- .gitignore (current)\n+++ .gitignore (init would write)\n\
             @@ -1,1 +1,2 @@\n target\n+.soothfast/\n"
        );
    }

    #[test]
    fn scaffolds_sharing_a_file_show_one_diff_of_what_init_writes() {
        let toml = |kind, content: &str| Scaffold {
            path: "soothfast.toml".into(),
            kind,
            content: content.into(),
        };
        let runner = toml(Kind::Runner, "[runner]\nlabels = [\"big\"]\n");
        let section = toml(Kind::Section, "[[project]]\ndir = \".\"\n");
        let held = "[gate]\nwalltime = \"hard\"\n";
        let diff = path_diff(&[&runner, &section], held);
        assert_eq!(diff.matches("--- soothfast.toml").count(), 1);
        assert!(diff.contains("+[runner]") && diff.contains("+[[project]]"));
        assert!(diff.contains("+labels = [\"big\"]") && diff.contains("+dir = \".\""));
    }
}
