//! `cargo soothfast init`: wire a cargo workspace into soothfast. The CI
//! workflow is derived from the workspace, so `init --check` fails once the
//! repo gains a bench target or docs markers the workflow does not run.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::{invoke, workspace};

const WORKFLOW: &str = include_str!("workflow.yml.tmpl");
const GATE_JOB: &str = include_str!("gate_job.yml.tmpl");
const DOCS_JOB: &str = include_str!("docs_job.yml.tmpl");
const LABELS_JOB: &str = include_str!("labels_job.yml.tmpl");

/// The labels soothfast acts on, with their colour and description.
const LABELS: &[(&str, &str, &str)] = &[
    (
        "soothfast:merge",
        "0e8a16",
        "Merge once the required checks pass",
    ),
    (
        "soothfast:update",
        "1d76db",
        "Keep this branch up to date with the default branch",
    ),
    (
        "soothfast:perf-accepted",
        "fbca04",
        "The gate's regressions are intended",
    ),
    ("do-not-merge", "b60205", "Cancel any queued merge"),
];

/// How often the labels job sweeps when nothing has pushed.
const SCHEDULE: &str = "  schedule:\n    - cron: \"*/30 * * * *\"\n";

const WORKFLOW_PATH: &str = ".github/workflows/soothfast.yml";

const SOOTHFAST_TOML: &str = "\
# Counters (callgrind or perf) gate; walltime is reported and never fails,
# which halves the merge-base rounds a gate measures.
[gate]
walltime = \"advisory\"
";

const BENCH_STUB: &str = "\
use @@crate@@ as _;

soothfast::bench_main!();
";

const BENCH_TARGET: &str = "
[[bench]]
name = \"soothfast\"
harness = false
";

/// How a scaffolded file is held to what init writes.
#[derive(Debug, PartialEq)]
enum Kind {
    /// Exactly this content; any difference is drift.
    Whole,
    /// Written once when absent, then the repo's own.
    Seed,
    /// One line the file must contain.
    Line,
}

#[derive(Debug)]
struct Scaffold {
    path: PathBuf,
    kind: Kind,
    content: String,
}

#[derive(Debug, PartialEq)]
enum State {
    Current,
    Missing,
    Differs,
}

impl Scaffold {
    fn state(&self, root: &Path) -> State {
        let Ok(text) = std::fs::read_to_string(root.join(&self.path)) else {
            return State::Missing;
        };
        match self.kind {
            Kind::Seed => State::Current,
            Kind::Whole if text == self.content => State::Current,
            Kind::Whole => State::Differs,
            Kind::Line if text.lines().any(|l| l.trim() == self.content) => State::Current,
            Kind::Line => State::Differs,
        }
    }

    fn write(&self, root: &Path) -> std::io::Result<()> {
        let path = root.join(&self.path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        match self.kind {
            Kind::Line => {
                let mut text = std::fs::read_to_string(&path).unwrap_or_default();
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&self.content);
                text.push('\n');
                std::fs::write(path, text)
            }
            Kind::Whole | Kind::Seed => std::fs::write(path, &self.content),
        }
    }
}

/// What the scaffold is derived from.
struct Workspace {
    default_branch: String,
    /// Packages with a `soothfast` bench target, in name order.
    benched: Vec<String>,
    /// The package `docs check` reads, when any markdown carries markers.
    docs_package: Option<String>,
    action: String,
    bot: bool,
}

pub fn run(args: &[String]) -> i32 {
    let mut check = false;
    let mut force = false;
    let mut bot = false;
    let mut labels = false;
    let mut bench: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--check" => check = true,
            "--force" => force = true,
            "--bot" => bot = true,
            "--labels" => labels = true,
            "-p" | "--package" => match it.next() {
                Some(p) => bench.push(p.clone()),
                None => return err("-p needs a package to add a bench target to"),
            },
            other => return err(&format!("unknown init arg {other:?}")),
        }
    }
    let root = match invoke::workspace_root() {
        Ok(r) => r,
        Err(e) => return err(&format!("not in a cargo workspace: {e}")),
    };
    let ws = match detect(&root, &bench, check, bot) {
        Ok(ws) => ws,
        Err(e) => return err(&e),
    };
    let files = scaffold(&ws);
    let rc = report(&root, &files, check, force);
    if labels && !check && rc == 0 {
        return create_labels();
    }
    rc
}

/// Create or refresh the labels on the repository `gh` resolves here.
fn create_labels() -> i32 {
    for (name, color, description) in LABELS {
        let status = Command::new("gh")
            .args(["label", "create", name, "--force", "--color", color])
            .args(["--description", description])
            .status();
        match status {
            Ok(s) if s.success() => println!("label    {name}"),
            _ => return err(&format!("could not create label {name} (is gh signed in?)")),
        }
    }
    0
}

/// What the scaffold derives from, after giving each of `bench` a bench
/// target unless only checking.
fn detect(root: &Path, bench: &[String], check: bool, bot: bool) -> Result<Workspace, String> {
    let load = || workspace::metadata(Some(root), true).map_err(|e| e.to_string());
    let mut meta = load()?;
    if !check && !bench.is_empty() {
        for pkg in bench {
            add_bench(&meta, pkg)?;
        }
        meta = load()?;
    }
    let existing = std::fs::read_to_string(root.join(WORKFLOW_PATH)).unwrap_or_default();
    Ok(Workspace {
        default_branch: default_branch(root, &existing),
        benched: benched(&meta),
        docs_package: has_markers(root).then(|| docs_package(&meta)).flatten(),
        action: action_ref(&existing),
        bot,
    })
}

fn report(root: &Path, files: &[Scaffold], check: bool, force: bool) -> i32 {
    let mut drift = 0;
    for f in files {
        let path = f.path.display();
        match (f.state(root), check) {
            (State::Current, _) => println!("ok       {path}"),
            (state, true) => {
                drift += 1;
                let what = if state == State::Missing {
                    "missing"
                } else {
                    "differs"
                };
                println!("{what:<8} {path}");
            }
            (State::Differs, false) if f.kind == Kind::Whole && !force => {
                drift += 1;
                println!("differs  {path} (left as is; --force rewrites it)");
            }
            (_, false) => match f.write(root) {
                Ok(()) => println!("wrote    {path}"),
                Err(e) => return err(&format!("cannot write {path}: {e}")),
            },
        }
    }
    if check && drift > 0 {
        println!("init: {drift} file(s) drifted from `cargo soothfast init`; run it to update");
        return 1;
    }
    if !check && drift == 0 {
        println!("init: done. Next: annotate code with #[soothfast::bench] and push.");
    }
    i32::from(drift > 0)
}

fn scaffold(ws: &Workspace) -> Vec<Scaffold> {
    vec![
        Scaffold {
            path: WORKFLOW_PATH.into(),
            kind: Kind::Whole,
            content: workflow(ws),
        },
        Scaffold {
            path: "soothfast.toml".into(),
            kind: Kind::Seed,
            content: SOOTHFAST_TOML.into(),
        },
        Scaffold {
            path: ".gitignore".into(),
            kind: Kind::Line,
            content: ".soothfast/".into(),
        },
    ]
}

fn workflow(ws: &Workspace) -> String {
    let mut text = WORKFLOW
        .replace("@@default_branch@@", &ws.default_branch)
        .replace("@@schedule@@", if ws.bot { SCHEDULE } else { "" });
    if !ws.benched.is_empty() {
        text.push_str(
            &GATE_JOB
                .replace("@@packages@@", &ws.benched.join(" "))
                .replace("@@changelog@@", if ws.bot { "true" } else { "false" }),
        );
    }
    if let Some(pkg) = &ws.docs_package {
        text.push_str(&DOCS_JOB.replace("@@docs_package@@", pkg));
    }
    if ws.bot {
        text.push_str(LABELS_JOB);
    }
    text.replace("@@action@@", &ws.action)
}

/// Members with a `soothfast` bench target.
fn benched(meta: &Value) -> Vec<String> {
    let mut names: Vec<String> = packages(meta)
        .filter_map(|p| p["name"].as_str())
        .filter(|name| workspace::bench_target_in(meta, name, "soothfast"))
        .map(String::from)
        .collect();
    names.sort();
    names
}

/// The workspace root's own package, else the first member by name.
fn docs_package(meta: &Value) -> Option<String> {
    let root = meta["workspace_root"].as_str()?;
    let manifest = Path::new(root).join("Cargo.toml");
    let mut names: Vec<(bool, String)> = packages(meta)
        .filter_map(|p| {
            let at_root = p["manifest_path"].as_str().map(Path::new) == Some(manifest.as_path());
            Some((!at_root, p["name"].as_str()?.to_string()))
        })
        .collect();
    names.sort();
    names.into_iter().next().map(|(_, n)| n)
}

fn packages(meta: &Value) -> impl Iterator<Item = &Value> {
    meta["packages"].as_array().into_iter().flatten()
}

/// Whether any tracked markdown carries a soothfast marker for `docs check`.
fn has_markers(root: &Path) -> bool {
    invoke::git_in(root, &["grep", "-l", "--", "<!-- soothfast:", "*.md"])
        .is_ok_and(|s| !s.is_empty())
}

/// The remote's HEAD, else the branch the workflow already pushes on, else
/// `main`. CI checkouts have no `origin/HEAD`, so the remote is asked.
fn default_branch(root: &Path, existing: &str) -> String {
    let git = |args: &[&str]| invoke::git_in(root, args).ok();
    git(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .and_then(|h| h.strip_prefix("origin/").map(String::from))
        .or_else(|| {
            git(&["ls-remote", "--symref", "origin", "HEAD"]).and_then(|t| symref_branch(&t))
        })
        .or_else(|| pushed_branch(existing))
        .unwrap_or_else(|| "main".into())
}

/// `ref: refs/heads/master\tHEAD` in `ls-remote --symref` output.
fn symref_branch(listing: &str) -> Option<String> {
    listing.lines().find_map(|l| {
        let (target, name) = l.strip_prefix("ref: ")?.split_once('\t')?;
        (name == "HEAD").then(|| target.strip_prefix("refs/heads/").map(String::from))?
    })
}

/// The branch a workflow init wrote pushes on.
fn pushed_branch(workflow: &str) -> Option<String> {
    let line = workflow
        .lines()
        .find(|l| l.trim_start().starts_with("branches: ["))?;
    let inner = line.split_once('[')?.1.split_once(']')?.0.trim();
    (!inner.is_empty()).then(|| inner.to_string())
}

/// This release of the action: the workflow's existing pin of it, else the
/// commit its tag names on the remote, else the tag.
fn action_ref(existing: &str) -> String {
    let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    let pinned = existing.lines().find_map(|l| {
        let pin = l.trim().strip_prefix("- uses: ")?.trim();
        (pin.starts_with("Verdenroz/soothfast@") && pin.ends_with(&format!(" # {tag}")))
            .then(|| pin.to_string())
    });
    if let Some(pin) = pinned {
        return pin;
    }
    let sha = Command::new("git")
        .args(["ls-remote", "https://github.com/Verdenroz/soothfast"])
        .arg(format!("refs/tags/{tag}"))
        .arg(format!("refs/tags/{tag}^{{}}"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| tag_commit(&String::from_utf8_lossy(&o.stdout)));
    match sha {
        Some(sha) => format!("Verdenroz/soothfast@{sha} # {tag}"),
        None => format!("Verdenroz/soothfast@{tag}"),
    }
}

/// The commit in `ls-remote` output: an annotated tag's peeled `^{}` line,
/// else the lightweight tag's own.
fn tag_commit(listing: &str) -> Option<String> {
    let refs: Vec<(&str, &str)> = listing
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter(|(sha, _)| sha.len() == 40)
        .collect();
    refs.iter()
        .find(|(_, r)| r.ends_with("^{}"))
        .or_else(|| refs.first())
        .map(|(sha, _)| sha.to_string())
}

/// Give `pkg` a `soothfast` bench target: the dev-dependency, the
/// `[[bench]]` table and a stub that links the library's registrations.
fn add_bench(meta: &Value, pkg: &str) -> Result<(), String> {
    let p = packages(meta)
        .find(|p| p["name"] == pkg)
        .ok_or_else(|| format!("no workspace member named {pkg:?}"))?;
    let manifest = PathBuf::from(p["manifest_path"].as_str().unwrap_or_default());
    let dir = manifest.parent().ok_or("package has no directory")?;
    let text = std::fs::read_to_string(&manifest).map_err(|e| e.to_string())?;
    if !text.contains("name = \"soothfast\"") {
        std::fs::write(&manifest, format!("{text}{BENCH_TARGET}")).map_err(|e| e.to_string())?;
        println!("wrote    {} ([[bench]] soothfast)", manifest.display());
    }
    let version = env!("CARGO_PKG_VERSION");
    let status = Command::new("cargo")
        .args(["add", "--dev", "-p", pkg, "--features", "runner"])
        .arg(format!("soothfast@{version}"))
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("cargo add soothfast@{version} failed for {pkg}"));
    }
    let stub = dir.join("benches/soothfast.rs");
    if !stub.exists() {
        std::fs::create_dir_all(dir.join("benches")).map_err(|e| e.to_string())?;
        let krate = pkg.replace('-', "_");
        std::fs::write(&stub, BENCH_STUB.replace("@@crate@@", &krate))
            .map_err(|e| e.to_string())?;
        println!("wrote    {}", stub.display());
    }
    Ok(())
}

fn err(msg: &str) -> i32 {
    eprintln!("init: {msg}");
    2
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ws(benched: &[&str], docs: Option<&str>) -> Workspace {
        Workspace {
            default_branch: "master".into(),
            benched: benched.iter().map(|s| s.to_string()).collect(),
            docs_package: docs.map(String::from),
            action: "Verdenroz/soothfast@v0".into(),
            bot: false,
        }
    }

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
    fn the_branch_comes_from_the_remote_or_the_workflow_already_written() {
        let listing = "ref: refs/heads/master\tHEAD\n0123\tHEAD\n";
        assert_eq!(symref_branch(listing).as_deref(), Some("master"));
        assert_eq!(symref_branch("0123\tHEAD\n"), None);
        let text = workflow(&ws(&[], None));
        assert_eq!(pushed_branch(&text).as_deref(), Some("master"));
    }

    #[test]
    fn an_existing_pin_of_this_release_is_kept() {
        let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
        let pin = format!("Verdenroz/soothfast@{} # {tag}", "c".repeat(40));
        let existing = format!("    steps:\n      - uses: {pin}\n");
        assert_eq!(action_ref(&existing), pin);
    }

    #[test]
    fn an_annotated_tag_pins_the_commit_it_peels_to() {
        let tag = "a".repeat(40);
        let commit = "b".repeat(40);
        let listing = format!("{tag}\trefs/tags/v1\n{commit}\trefs/tags/v1^{{}}\n");
        assert_eq!(tag_commit(&listing), Some(commit.clone()));
        assert_eq!(
            tag_commit(&format!("{commit}\trefs/tags/v1\n")),
            Some(commit)
        );
        assert_eq!(tag_commit(""), None);
    }

    #[test]
    fn the_bot_adds_a_schedule_and_the_labels_job() {
        let plain = workflow(&ws(&[], None));
        assert!(!plain.contains("schedule:") && !plain.contains("labels: \"true\""));
        let bot = workflow(&Workspace {
            bot: true,
            ..ws(&["a"], None)
        });
        assert!(bot.contains("  schedule:\n    - cron:"));
        assert!(bot.contains("\n  labels:\n"));
        assert!(bot.contains("changelog: \"true\""));
        assert!(!bot.contains("@@"));
    }

    #[test]
    fn only_bench_targets_named_soothfast_count() {
        let meta = json!({ "packages": [
            { "name": "b", "targets": [{ "name": "soothfast", "kind": ["bench"] }] },
            { "name": "a", "targets": [{ "name": "soothfast", "kind": ["bench"] }] },
            { "name": "c", "targets": [{ "name": "other", "kind": ["bench"] }] },
        ]});
        assert_eq!(benched(&meta), ["a", "b"]);
    }

    #[test]
    fn the_root_package_documents_the_workspace() {
        let meta = json!({ "workspace_root": "/w", "packages": [
            { "name": "a", "manifest_path": "/w/a/Cargo.toml" },
            { "name": "z", "manifest_path": "/w/Cargo.toml" },
        ]});
        assert_eq!(docs_package(&meta).as_deref(), Some("z"));
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("soothfast-init-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
        assert_eq!(seed.state(&root), State::Missing);
        assert_eq!(line.state(&root), State::Differs);
        seed.write(&root).unwrap();
        line.write(&root).unwrap();
        std::fs::write(root.join("s.toml"), "edited").unwrap();
        assert_eq!(seed.state(&root), State::Current);
        assert_eq!(line.state(&root), State::Current);
        assert_eq!(
            std::fs::read_to_string(root.join(".gitignore")).unwrap(),
            "target\n.soothfast/\n"
        );
    }

    #[test]
    fn check_reports_drift_and_writes_nothing() {
        let root = scratch("check");
        let files = scaffold(&ws(&["a"], None));
        assert_eq!(report(&root, &files, true, false), 1);
        assert!(!root.join(WORKFLOW_PATH).exists());
        assert_eq!(report(&root, &files, false, false), 0);
        assert_eq!(report(&root, &files, true, false), 0);
    }

    #[test]
    fn an_edited_workflow_is_kept_unless_forced() {
        let root = scratch("force");
        let files = scaffold(&ws(&[], None));
        report(&root, &files, false, false);
        std::fs::write(root.join(WORKFLOW_PATH), "mine").unwrap();
        assert_eq!(report(&root, &files, false, false), 1);
        assert_eq!(
            std::fs::read_to_string(root.join(WORKFLOW_PATH)).unwrap(),
            "mine"
        );
        assert_eq!(report(&root, &files, false, true), 0);
        assert_ne!(
            std::fs::read_to_string(root.join(WORKFLOW_PATH)).unwrap(),
            "mine"
        );
    }
}
