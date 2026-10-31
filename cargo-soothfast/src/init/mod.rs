//! `cargo soothfast init`: wire a repo into soothfast. The CI workflow is
//! derived from the cargo workspace at the root and the other-language
//! projects below it, so `init --check` fails once the repo gains a bench
//! target, a project or docs markers the workflow does not run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::{invoke, workspace};

pub mod ci;
mod detect;
pub(crate) mod ecosystem;
mod textdiff;

use ci::Ci;
use ecosystem::{
    Ecosystem, Entry, Project, Settings, Suite, normalize_dir, parse_projects, parse_suites,
};

const WORKFLOW: &str = include_str!("workflow.yml.tmpl");
const TEST_JOB: &str = include_str!("test_job.yml.tmpl");
const GATE_JOB: &str = include_str!("gate_job.yml.tmpl");
const CALLER_JOB: &str = include_str!("caller_job.yml.tmpl");
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

const IMPORT_TOML: &str = "\
# Imported gates measure walltime only, both sides on the same runner.
# Set it to \"advisory\" if the runner is too noisy to trust.
[gate]
walltime = \"hard\"
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
    /// The `[[project]]` tables, replaced when their settings change.
    Section,
    /// The `[runner]` table, replaced when its labels change.
    Runner,
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
            Kind::Section if parse_projects(&text) == parse_projects(&self.content) => {
                State::Current
            }
            Kind::Section => State::Differs,
            Kind::Runner if labels(&text) == labels(&self.content) => State::Current,
            Kind::TestCi if test_ci(&text) == test_ci(&self.content) => State::Current,
            Kind::Docs if docs(&text) == docs(&self.content) => State::Current,
            Kind::Runner | Kind::TestCi | Kind::Docs => State::Differs,
        }
    }

    /// Whether the file still saves projects as an `[init]` or
    /// `[[init.project]]` table.
    fn holds_legacy(&self, root: &Path) -> bool {
        let text = std::fs::read_to_string(root.join(&self.path)).unwrap_or_default();
        self.kind == Kind::Section
            && text.lines().any(|l| {
                matches!(
                    ecosystem::table_name(l),
                    Some("init" | ecosystem::LEGACY_PROJECT_TABLE)
                )
            })
    }

    /// The file as `init` leaves it, given what it holds now.
    fn updated(&self, current: &str) -> String {
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

    fn write(&self, root: &Path) -> std::io::Result<()> {
        let path = root.join(&self.path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        std::fs::write(path, self.updated(&current))
    }

    /// What `write` would change in the file, as a unified diff.
    fn diff(&self, root: &Path) -> String {
        let current = std::fs::read_to_string(root.join(&self.path)).unwrap_or_default();
        let hunks = textdiff::unified(&current, &self.updated(&current));
        let path = self.path.display();
        format!("--- {path} (current)\n+++ {path} (init would write)\n{hunks}")
    }
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

/// What the scaffold is derived from.
struct Workspace {
    /// Whether the root is a cargo workspace.
    rust: bool,
    /// The non-Rust projects, in directory order.
    projects: Vec<Project>,
    default_branch: String,
    ci: Ci,
    /// Packages with a `soothfast` bench target, in name order.
    benched: Vec<String>,
    /// The packages `docs check` reads, when any markdown carries markers.
    docs_packages: Vec<String>,
    action: String,
    bot: bool,
    java: String,
    node: String,
}

/// The toolchain versions and soothfast ref init's flags pin, over the ones an existing
/// workflow already carries.
#[derive(Default)]
struct Pins {
    java: Option<String>,
    node: Option<String>,
    /// The soothfast ref the workflow uses, over this release's tag.
    action: Option<String>,
}

pub fn run(args: &[String]) -> i32 {
    let mut check = false;
    let mut force = false;
    let mut bot = false;
    let mut labels = false;
    let mut bench: Vec<String> = Vec::new();
    let mut flags = Settings::default();
    let mut dir: Option<String> = None;
    let mut ci = Ci::default();
    let mut tools = Pins::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--help" | "-h" => {
                println!("{}", init_usage());
                return 0;
            }
            "--check" => check = true,
            "--force" => force = true,
            "--bot" => bot = true,
            "--labels" => labels = true,
            "-p" | "--package" => match it.next() {
                Some(p) => bench.push(p.clone()),
                None => return err("-p needs a package to add a bench target to"),
            },
            "--ecosystem" => match it.next().map(|e| Ecosystem::parse(e)) {
                Some(Ok(e)) => flags.ecosystem = Some(e),
                Some(Err(e)) => return err(&e),
                None => return err("--ecosystem needs rust, gradle, maven, python or node"),
            },
            "--runner-labels" => match it.next() {
                Some(labels) => ci.labels = Some(labels.split(',').map(String::from).collect()),
                None => return err("--runner-labels needs comma-separated runner labels"),
            },
            "--no-test" => ci.test = Some(false),
            "--dir" => match it.next().map(|d| normalize_dir(d)) {
                Some(Ok(d)) => dir = Some(d),
                Some(Err(e)) => return err(&e),
                None => return err("--dir needs a directory"),
            },
            "--pin" => match it.next() {
                Some(r)
                    if r.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c)) =>
                {
                    tools.action = Some(r.clone())
                }
                _ => return err("--pin needs a commit, tag or branch of soothfast"),
            },
            "--java" | "--node" => {
                let Some(version) = it.next().cloned() else {
                    return err(&format!("{a} needs a version"));
                };
                match a.as_str() {
                    "--java" => tools.java = Some(version),
                    _ => tools.node = Some(version),
                }
            }
            "--bench-cmd" | "--bench-out" | "--from" => {
                let Some(value) = it.next().cloned() else {
                    return err(&format!("{a} needs a value"));
                };
                match a.as_str() {
                    "--bench-cmd" => flags.bench_cmd = Some(value),
                    "--bench-out" => flags.bench_out = Some(value),
                    _ => flags.from = Some(value),
                }
            }
            other => return err(&format!("unknown init arg {other:?}")),
        }
    }
    let (root, ws, notes) = match target(flags, dir, ci, tools, &bench, check, bot) {
        Ok(t) => t,
        Err(e) => return err(&e),
    };
    for note in &notes {
        println!("{note}");
    }
    let callers = other_callers(&root);
    for caller in &callers {
        println!(
            "warn     {caller} calls soothfast; its jobs run twice until it or {WORKFLOW_PATH} is removed"
        );
    }
    let duplicate_only = ws.projects.is_empty() && ws.ci.is_default();
    if !callers.is_empty() && duplicate_only && !force && !root.join(WORKFLOW_PATH).exists() {
        println!(
            "init: nothing written, since {WORKFLOW_PATH} would only duplicate the jobs above; --force writes it"
        );
        return 0;
    }
    let files = scaffold(&ws);
    let rc = report(&root, &files, check, force);
    if check || rc != 0 {
        return rc;
    }
    println!("init: done. Next: {}", next_step(ws.rust));
    if labels {
        return create_labels();
    }
    0
}

/// The workflow files besides the one init writes that call soothfast, as
/// repo-relative paths.
fn other_callers(root: &Path) -> Vec<String> {
    let dir = root.join(".github/workflows");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let calls = |text: &str| {
        text.lines().any(|l| {
            let uses = l.trim().trim_start_matches("- ").strip_prefix("uses:");
            let target = uses.map(str::trim);
            target.is_some_and(|t| {
                t.strip_prefix("Verdenroz/soothfast")
                    .is_some_and(|rest| rest.starts_with(['@', '/']))
            })
        })
    };
    let mut found: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
        .filter(|p| p.file_name() != Path::new(WORKFLOW_PATH).file_name())
        .filter(|p| std::fs::read_to_string(p).is_ok_and(|t| calls(&t)))
        .filter_map(|p| Some(format!(".github/workflows/{}", p.file_name()?.to_str()?)))
        .collect();
    found.sort();
    found
}

/// The `init` entry of the CLI usage text.
fn init_usage() -> String {
    let lines = crate::USAGE
        .lines()
        .skip_while(|l| !l.starts_with("  init "));
    let rest = lines.skip(1).take_while(|l| l.starts_with("    "));
    let first = crate::USAGE.lines().find(|l| l.starts_with("  init "));
    first.into_iter().chain(rest).collect::<Vec<_>>().join("\n")
}

/// What to do after `init` before the first pull request gates anything.
fn next_step(rust: bool) -> &'static str {
    if rust {
        "annotate code with #[soothfast::bench] and push."
    } else {
        "check that the bench command writes its result file, then push."
    }
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

/// The project root, what its scaffold derives from, and a line per
/// project found.
fn target(
    flags: Settings,
    dir: Option<String>,
    ci: Ci,
    tools: Pins,
    bench: &[String],
    check: bool,
    bot: bool,
) -> Result<(PathBuf, Workspace, Vec<String>), String> {
    let root = invoke::project_root();
    let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap_or_default();
    let stored = parse_projects(&text).map_err(|e| format!("soothfast.toml: {e}"))?;
    let ci = ci.over(Ci::parse(&text).map_err(|e| format!("soothfast.toml: {e}"))?);
    let root_is_rust = flags.ecosystem == Some(Ecosystem::Rust);
    if root_is_rust && dir.is_some() {
        return Err("--ecosystem rust is the cargo workspace at the root; drop --dir".into());
    }
    let flags = if root_is_rust {
        Settings::default()
    } else {
        flags
    };
    let rust = root_is_rust || root.join("Cargo.toml").is_file();
    let (projects, notes) = plan(&root, stored, flags, dir)?;
    if !rust && projects.is_empty() {
        return Err(if notes.is_empty() {
            "no Cargo.toml, build.gradle(.kts), pom.xml, pyproject.toml, setup.py or \
             package.json found; pass --ecosystem"
                .into()
        } else {
            notes.join("\n")
        });
    }
    let markers = has_markers(&root);
    let (benched, docs_packages) = if rust {
        invoke::workspace_root().map_err(|e| format!("not in a cargo workspace: {e}"))?;
        let meta = rust_meta(&root, bench, check)?;
        let docs = if markers {
            docs_packages(&root, &meta, &ci)
        } else {
            Vec::new()
        };
        (benched(&meta), docs)
    } else if bench.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        return Err("-p adds a bench target to a cargo package".into());
    };
    let existing = std::fs::read_to_string(root.join(WORKFLOW_PATH)).unwrap_or_default();
    let ws = Workspace {
        rust,
        projects,
        ci,
        default_branch: default_branch(&root, &existing),
        benched,
        docs_packages,
        action: action_ref(&existing, tools.action.as_deref()),
        bot,
        java: tools
            .java
            .or_else(|| pinned(&existing, "java"))
            .unwrap_or("21".into()),
        node: tools
            .node
            .or_else(|| pinned(&existing, "node"))
            .unwrap_or("22".into()),
    };
    Ok((root, ws, notes))
}

/// The non-Rust projects to gate, and a line per project found: saved
/// settings over what the tracked files show, and the flags over both. A
/// project found only by detection is skipped when it has no benchmark
/// command.
fn plan(
    root: &Path,
    stored: Vec<Entry>,
    flags: Settings,
    dir: Option<String>,
) -> Result<(Vec<Project>, Vec<String>), String> {
    let detected = detect::projects(root);
    let idle: BTreeSet<String> = detected
        .iter()
        .filter(|f| !f.harness)
        .map(|f| f.dir.clone())
        .collect();
    let mut settings: BTreeMap<String, Settings> = detected
        .into_iter()
        .map(|f| {
            let found = Settings {
                ecosystem: Some(f.ecosystem),
                ..Settings::default()
            };
            (f.dir, found)
        })
        .collect();
    let mut chosen: BTreeSet<String> = BTreeSet::new();
    for Entry {
        dir,
        settings: saved,
    } in stored
    {
        let found = settings.remove(&dir).unwrap_or_default();
        settings.insert(dir.clone(), saved.over(found));
        chosen.insert(dir);
    }
    if flags != Settings::default() || dir.is_some() {
        let dir = dir.unwrap_or_else(|| ".".into());
        let current = settings.remove(&dir).unwrap_or_default();
        settings.insert(dir.clone(), flags.over(current));
        chosen.insert(dir);
    }
    let mut projects = Vec::new();
    let mut notes = Vec::new();
    for (dir, found) in settings {
        let Some(ecosystem) = found.ecosystem else {
            return Err(format!("no project detected in {dir}; pass --ecosystem"));
        };
        if idle.contains(&dir) && !chosen.contains(&dir) {
            notes.push(format!(
                "skipped  {dir} ({}): no benchmark harness found; add --dir {dir} \
                 --bench-cmd CMD --bench-out PATH",
                ecosystem.name()
            ));
            continue;
        }
        match found.bench_for(ecosystem) {
            Ok(bench) => {
                notes.push(format!("project  {dir} ({})", ecosystem.name()));
                projects.push(Project {
                    ecosystem,
                    dir,
                    bench,
                    test: found.tests(),
                    walltime: found.walltime.or(Some("hard".into())),
                    prefix: found.prefix,
                });
            }
            Err(e) if chosen.contains(&dir) => return Err(format!("{dir}: {e}")),
            Err(_) => notes.push(format!(
                "skipped  {dir} ({}): no benchmark command; add --dir {dir} \
                 --bench-cmd CMD --bench-out PATH",
                ecosystem.name()
            )),
        }
    }
    Ok((projects, notes))
}

/// The cargo metadata of the workspace at `root`, after giving each of
/// `bench` a bench target unless only checking.
fn rust_meta(root: &Path, bench: &[String], check: bool) -> Result<Value, String> {
    let load = || workspace::metadata(Some(root), true).map_err(|e| e.to_string());
    let meta = load()?;
    if check || bench.is_empty() {
        return Ok(meta);
    }
    for pkg in bench {
        add_bench(&meta, pkg)?;
    }
    load()
}

fn report(root: &Path, files: &[Scaffold], check: bool, force: bool) -> i32 {
    let mut drift = 0;
    let writing = !check && files.iter().any(|f| f.state(root) != State::Current);
    for f in files {
        let path = match f.kind {
            Kind::Section => format!("{} [[project]]", f.path.display()),
            Kind::Runner => format!("{} [runner]", f.path.display()),
            Kind::TestCi => format!("{} [test] ci", f.path.display()),
            Kind::Docs => format!("{} [docs] package", f.path.display()),
            _ => f.path.display().to_string(),
        };
        let state = match f.state(root) {
            State::Current if writing && f.holds_legacy(root) => State::Differs,
            state => state,
        };
        match (state, check) {
            (State::Current, _) => println!("ok       {path}"),
            (state, true) => {
                drift += 1;
                let what = if state == State::Missing {
                    "missing"
                } else {
                    "differs"
                };
                println!("{what:<8} {path}");
                if state == State::Differs {
                    print!("{}", f.diff(root));
                }
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
    i32::from(drift > 0)
}

fn scaffold(ws: &Workspace) -> Vec<Scaffold> {
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

fn workflow(ws: &Workspace) -> String {
    let mut text = WORKFLOW
        .replace("@@default_branch@@", &ws.default_branch)
        .replace("@@schedule@@", if ws.bot { SCHEDULE } else { "" });
    if ws.projects.is_empty() && ws.ci.is_default() {
        text.push_str(&cargo_jobs(ws));
        if ws.bot {
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
    text
}

/// The value of `key: "..."` in a workflow init wrote.
fn pinned(workflow: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}: \"");
    workflow.lines().find_map(|l| {
        let rest = l.trim().strip_prefix(&prefix)?;
        Some(rest.strip_suffix('"')?.to_string())
    })
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

/// The packages `docs check` reads: `[docs] package` when set, else the
/// workspace packages the markdown binds name, else the root's own package.
fn docs_packages(root: &Path, meta: &Value, ci: &Ci) -> Vec<String> {
    if let Some(packages) = &ci.docs {
        return packages.clone();
    }
    let listing = invoke::git_in(root, &["ls-files", "-z", "--", "*.md"]).unwrap_or_default();
    let prefixes: BTreeSet<String> = listing
        .split('\0')
        .filter(|path| !path.is_empty())
        .filter_map(|path| std::fs::read_to_string(root.join(path)).ok())
        .flat_map(|text| bind_crates(&text))
        .collect();
    let bound: Vec<String> = packages(meta)
        .filter_map(|p| p["name"].as_str())
        .filter(|name| prefixes.contains(&name.replace('-', "_")))
        .map(String::from)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if bound.is_empty() {
        docs_package(meta).into_iter().collect()
    } else {
        bound
    }
}

/// The crate part of each `crate::path` bind in `text`.
fn bind_crates(text: &str) -> Vec<String> {
    text.split("<!-- soothfast:bind ")
        .skip(1)
        .filter_map(|rest| rest.split_whitespace().next())
        .filter_map(|target| Some(target.split_once("::")?.0.to_string()))
        .collect()
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
pub fn has_markers(root: &Path) -> bool {
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

/// The `--pin` ref, else the workflow's existing pin of this release or of a
/// bare commit, else the commit this release's tag names on the remote, else
/// the tag.
fn action_ref(existing: &str, pin: Option<&str>) -> String {
    if let Some(pin) = pin {
        return format!("Verdenroz/soothfast@{pin}");
    }
    let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    let pinned = existing.lines().find_map(|l| {
        let pin = l
            .trim()
            .trim_start_matches("- ")
            .strip_prefix("uses: ")?
            .trim();
        let pin = pin.replacen("/.github/workflows/run.yml", "", 1);
        let commit = pin
            .strip_prefix("Verdenroz/soothfast@")
            .is_some_and(|r| r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit()));
        (pin.starts_with("Verdenroz/soothfast@") && (pin.ends_with(&format!(" # {tag}")) || commit))
            .then_some(pin)
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

/// The runner labels and `[test] ci` setting of the repo at `root`.
pub fn ci(root: &Path) -> Result<Ci, String> {
    let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap_or_default();
    Ci::parse(&text).map_err(|e| format!("soothfast.toml: {e}"))
}

/// The packages and docs packages `plan` reads from a cargo workspace.
pub struct RustPlan {
    pub packages: Vec<String>,
    pub docs_packages: Vec<String>,
}

pub fn rust_plan(root: &Path) -> Result<RustPlan, String> {
    let meta = workspace::metadata(Some(root), true).map_err(|e| e.to_string())?;
    let docs = if has_markers(root) {
        docs_packages(root, &meta, &ci(root)?)
    } else {
        Vec::new()
    };
    Ok(RustPlan {
        packages: benched(&meta),
        docs_packages: docs,
    })
}

/// The non-Rust projects `soothfast.toml` saves, with their benchmark
/// settings resolved.
pub fn manifest(root: &Path) -> Result<Vec<Project>, String> {
    let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap_or_default();
    let entries = parse_projects(&text).map_err(|e| format!("soothfast.toml: {e}"))?;
    entries
        .into_iter()
        .map(|Entry { dir, settings }| {
            let ecosystem = settings
                .ecosystem
                .ok_or_else(|| format!("{dir}: a project needs an ecosystem"))?;
            let bench = settings
                .bench_for(ecosystem)
                .map_err(|e| format!("{dir}: {e}"))?;
            Ok(Project {
                ecosystem,
                dir,
                bench,
                test: settings.tests(),
                walltime: settings.walltime.or(Some("hard".into())),
                prefix: settings.prefix,
            })
        })
        .collect()
}

/// The `[[test.suite]]` tables saved in `soothfast.toml`.
pub fn suites(root: &Path) -> Result<Vec<Suite>, String> {
    let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap_or_default();
    parse_suites(&text).map_err(|e| format!("soothfast.toml: {e}"))
}

fn err(msg: &str) -> i32 {
    eprintln!("init: {msg}");
    2
}

#[cfg(test)]
mod tests {
    use super::ecosystem::Tests;
    use super::*;
    use serde_json::json;

    fn ws(benched: &[&str], docs: Option<&str>) -> Workspace {
        Workspace {
            rust: true,
            projects: Vec::new(),
            ci: Ci::default(),
            default_branch: "master".into(),
            benched: benched.iter().map(|s| s.to_string()).collect(),
            docs_packages: docs.map(String::from).into_iter().collect(),
            action: "Verdenroz/soothfast@v0".into(),
            bot: false,
            java: "21".into(),
            node: "22".into(),
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
        assert_eq!(action_ref(&existing, None), pin);
    }

    #[test]
    fn a_pin_overrides_and_a_bare_commit_pin_is_kept() {
        assert_eq!(action_ref("", Some("abc123")), "Verdenroz/soothfast@abc123");
        let sha = "e".repeat(40);
        let existing = format!("    uses: Verdenroz/soothfast/.github/workflows/run.yml@{sha}\n");
        assert_eq!(
            action_ref(&existing, None),
            format!("Verdenroz/soothfast@{sha}")
        );
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
    fn binds_name_the_crates_that_hold_their_items() {
        let text = "<!-- soothfast:bind my_core::a::B -->\nprose\n<!-- /soothfast:bind -->\n\
                    <!-- soothfast:bind other::c -->\n<!-- soothfast:bind src/x.py#f -->\n";
        assert_eq!(bind_crates(text), ["my_core", "other"]);
    }

    #[test]
    fn the_docs_packages_are_written_and_a_flagless_check_is_clean() {
        let root = scratch("docs");
        std::fs::write(root.join("soothfast.toml"), "[docs]\npaths = [\"x\"]\n").unwrap();
        let w = Workspace {
            docs_packages: vec!["a".into(), "b".into()],
            ..ws(&[], None)
        };
        let files = scaffold(&w);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert!(text.contains("package = [\"a\",\"b\"]") && text.contains("paths = [\"x\"]"));
        assert_eq!(report(&root, &files, true, false), 0);
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
    fn a_drifted_pin_shows_as_a_changed_line() {
        let root = scratch("diff-pin");
        let workflow = Scaffold {
            path: "ci.yml".into(),
            kind: Kind::Whole,
            content: "jobs:\n  gate:\n    uses: acme/soothfast@v1\n    with: {}\n".into(),
        };
        std::fs::write(
            root.join("ci.yml"),
            "jobs:\n  gate:\n    uses: acme/soothfast@v0\n    with: {}\n",
        )
        .unwrap();
        assert_eq!(
            workflow.diff(&root),
            "--- ci.yml (current)\n+++ ci.yml (init would write)\n\
             @@ -1,4 +1,4 @@\n jobs:\n   gate:\n-    uses: acme/soothfast@v0\n\
             +    uses: acme/soothfast@v1\n     with: {}\n"
        );
        assert_eq!(report(&root, &[workflow], true, false), 1);
    }

    #[test]
    fn a_missing_ignore_line_shows_as_an_added_line() {
        let root = scratch("diff-line");
        let ignore = Scaffold {
            path: ".gitignore".into(),
            kind: Kind::Line,
            content: ".soothfast/".into(),
        };
        std::fs::write(root.join(".gitignore"), "target\n").unwrap();
        assert_eq!(
            ignore.diff(&root),
            "--- .gitignore (current)\n+++ .gitignore (init would write)\n\
             @@ -1,1 +1,2 @@\n target\n+.soothfast/\n"
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

    fn project(eco: Ecosystem, dir: &str) -> Project {
        let settings = Settings {
            bench_cmd: (eco == Ecosystem::Gradle).then(|| "./gradlew jmh -Pout={out}".into()),
            ..Settings::default()
        };
        Project {
            ecosystem: eco,
            dir: dir.into(),
            bench: settings.bench(eco).ok(),
            walltime: None,
            prefix: None,
            test: Tests::default(),
        }
    }

    fn python_ws() -> Workspace {
        Workspace {
            rust: false,
            projects: vec![project(Ecosystem::Python, ".")],
            ..ws(&[], None)
        }
    }

    #[test]
    fn the_init_table_makes_a_flagless_check_clean() {
        let root = scratch("table");
        let files = scaffold(&python_ws());
        assert_eq!(report(&root, &files, true, false), 1);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        let entry = parse_projects(&text).unwrap().remove(0);
        let bench = entry.settings.bench(Ecosystem::Python).unwrap();
        assert_eq!(
            (entry.dir.as_str(), bench),
            (".", python_ws().projects.remove(0).bench.unwrap())
        );
        assert_eq!(report(&root, &files, true, false), 0);
    }

    #[test]
    fn a_changed_bench_rewrites_the_table_and_keeps_the_rest() {
        let root = scratch("rewrite");
        std::fs::write(
            root.join("soothfast.toml"),
            "[gate]\nwalltime = \"hard\"\n\n[init]\nfrom = \"pyperf\"\n\n[site]\nname = \"x\"\n",
        )
        .unwrap();
        let files = scaffold(&python_ws());
        assert_eq!(report(&root, &files, true, false), 1);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert!(text.contains("walltime = \"hard\"") && text.contains("name = \"x\""));
        assert_eq!(text.matches("[[project]]").count(), 1);
        assert!(!text.contains("pyperf"));
        assert_eq!(report(&root, &files, true, false), 0);
    }

    #[test]
    fn a_padded_commented_init_header_is_replaced_in_place() {
        let root = scratch("padded");
        std::fs::write(
            root.join("soothfast.toml"),
            "[ init ] # managed\nfrom = \"pyperf\"\n\n[site]\nname = \"x\"\n",
        )
        .unwrap();
        let files = scaffold(&python_ws());
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert_eq!(text.matches("[[project]]").count(), 1);
        assert!(!text.contains("[ init ]") && !text.contains("pyperf"));
        assert!(parse_projects(&text).is_ok());
    }

    fn mixed_ws(rust: bool, projects: Vec<Project>) -> Workspace {
        Workspace {
            rust,
            projects,
            ..ws(&["a"], None)
        }
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
        assert_eq!(action_ref(&workflow(&w), None), action);
    }

    #[test]
    fn the_manifest_holds_every_project_and_a_flagless_check_is_clean() {
        let root = scratch("mixed");
        let w = mixed_ws(
            true,
            vec![
                project(Ecosystem::Python, "plugin/x"),
                project(Ecosystem::Gradle, "client"),
            ],
        );
        let files = scaffold(&w);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert_eq!(text.matches("[[project]]").count(), 2);
        assert!(text.contains("walltime = \"advisory\""));
        assert_eq!(report(&root, &files, true, false), 0);
    }

    #[test]
    fn a_legacy_init_table_is_current_but_rewritten_when_init_writes() {
        let root = scratch("legacy");
        let files = scaffold(&python_ws());
        report(&root, &files, false, false);
        let legacy = Settings::default().bench(Ecosystem::Python).unwrap();
        std::fs::write(
            root.join("soothfast.toml"),
            format!(
                "[init]\necosystem = \"python\"\nbench_cmd = \"{}\"\nbench_out = \"{}\"\nfrom = \"{}\"\n",
                legacy.cmd, legacy.out, legacy.from
            ),
        )
        .unwrap();
        assert_eq!(report(&root, &files, true, false), 0);
        std::fs::remove_file(root.join(".gitignore")).unwrap();
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert!(text.contains("[[project]]") && !text.contains("[init]"));
    }

    #[test]
    fn a_saved_project_is_kept_over_detection_and_flags_win() {
        let root = scratch("plan");
        let git = |args: &[&str]| invoke::git_in(&root, args).unwrap();
        git(&["init", "-q"]);
        std::fs::create_dir_all(root.join("client")).unwrap();
        std::fs::write(root.join("client/settings.gradle.kts"), "").unwrap();
        let bench = r#"{"scripts":{"bench":"vitest bench"},"devDependencies":{"vitest":"1"}}"#;
        std::fs::write(root.join("package.json"), bench).unwrap();
        std::fs::create_dir_all(root.join("tools")).unwrap();
        std::fs::write(
            root.join("tools/pyproject.toml"),
            "[project]\nname = \"tools\"\n",
        )
        .unwrap();
        git(&["add", "-A"]);
        let (projects, notes) = plan(&root, Vec::new(), Settings::default(), None).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].dir, ".");
        assert_eq!(projects[0].walltime.as_deref(), Some("hard"));
        assert!(notes.iter().any(|n| {
            n.starts_with("skipped  tools (python): no benchmark harness found; add --dir tools")
        }));
        assert!(
            notes
                .iter()
                .any(|n| n.starts_with("skipped  client (gradle)"))
        );
        assert!(
            notes
                .iter()
                .any(|n| n.contains("--dir client --bench-cmd CMD"))
        );

        let flags = Settings {
            bench_cmd: Some("./gradlew jmh -Pout={out}".into()),
            ..Settings::default()
        };
        let (projects, _) = plan(&root, Vec::new(), flags, Some("client".into())).unwrap();
        let dirs: Vec<_> = projects.iter().map(|p| p.dir.as_str()).collect();
        assert_eq!(dirs, [".", "client"]);
        assert_eq!(
            projects[1].bench.as_ref().unwrap().cmd,
            "./gradlew jmh -Pout={out}"
        );

        let by_flag = Settings {
            ecosystem: Some(Ecosystem::Python),
            ..Settings::default()
        };
        let (projects, _) = plan(&root, Vec::new(), by_flag, Some("plugin/hooks".into())).unwrap();
        assert!(
            projects
                .iter()
                .any(|p| p.dir == "plugin/hooks" && p.ecosystem == Ecosystem::Python)
        );

        let none = plan(
            &root,
            Vec::new(),
            Settings::default(),
            Some("nowhere".into()),
        );
        assert!(none.unwrap_err().contains("nowhere"));
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

    #[test]
    fn the_runner_and_test_settings_are_saved_and_a_flagless_check_is_clean() {
        let root = scratch("ci");
        std::fs::write(
            root.join("soothfast.toml"),
            "[test]\nignore = [\"docs/**\"]\nci = true\n\n[site]\nname = \"x\"\n",
        )
        .unwrap();
        let w = Workspace {
            ci: Ci {
                labels: Some(vec!["corvid".into()]),
                test: Some(false),
                docs: None,
            },
            ..ws(&["a"], None)
        };
        let files = scaffold(&w);
        assert_eq!(report(&root, &files, true, false), 1);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert_eq!(text.matches("[runner]").count(), 1);
        assert_eq!(text.matches("ci = ").count(), 1);
        assert!(text.contains("ignore = [\"docs/**\"]") && text.contains("ci = false"));
        assert!(text.contains("name = \"x\""));
        assert_eq!(report(&root, &files, true, false), 0);
    }

    #[test]
    fn other_workflows_that_call_soothfast_are_found() {
        let root = scratch("callers");
        let dir = root.join(".github/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        write(
            "ci.yml",
            "    steps:\n      - uses: Verdenroz/soothfast@abc # v1\n",
        );
        write(
            "reuse.yaml",
            "    uses: Verdenroz/soothfast/.github/workflows/run.yml@abc\n",
        );
        write("soothfast.yml", "    uses: Verdenroz/soothfast@abc\n");
        write(
            "other.yml",
            "      - uses: actions/checkout@abc\n# Verdenroz/soothfast@x\n",
        );
        assert_eq!(
            other_callers(&root),
            [".github/workflows/ci.yml", ".github/workflows/reuse.yaml"]
        );
        assert!(other_callers(&scratch("none")).is_empty());
    }

    #[test]
    fn the_init_usage_lists_init_alone() {
        let usage = init_usage();
        assert!(usage.starts_with("  init ") && usage.contains("--dir D"));
        assert!(usage.contains("--runner-labels") && !usage.contains("import "));
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
