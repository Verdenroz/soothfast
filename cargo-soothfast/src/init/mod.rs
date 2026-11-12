//! `cargo soothfast init`: wire a repo into soothfast. The CI workflow is
//! derived from the cargo workspace at the root and the other-language
//! projects below it, so `init --check` fails once the repo gains a bench
//! target, a project or docs markers the workflow does not run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::invoke;

pub(crate) mod bot;
pub mod ci;
mod detect;
pub(crate) mod ecosystem;
mod labels;
mod lookup;
mod rust;
mod scaffold;
mod textdiff;
mod workflow;

#[cfg(test)]
mod no_cargo_tests;
#[cfg(test)]
mod test_support;

use bot::has_bot_table;
use ci::Ci;
use ecosystem::{
    Ecosystem, Entry, Project, Settings, Suite, normalize_dir, parse_projects, parse_suites,
};
use labels::{create_labels, label_exit};
use lookup::{action_ref, default_branch, pinned};
pub(crate) use rust::benched;
pub use rust::{RustPlan, has_markers, rust_plan};
use rust::{docs_packages, rust_meta};
use scaffold::{Kind, Scaffold, State, path_diff, read_text, scaffold};
use workflow::{WORKFLOW_PATH, Workspace};

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
    run_at(&invoke::project_root(), args)
}

fn run_at(root: &Path, args: &[String]) -> i32 {
    let mut check = false;
    let mut force = false;
    let mut bot = false;
    let mut no_bot = false;
    let mut labels = false;
    let mut no_labels = false;
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
            "--no-bot" => no_bot = true,
            "--labels" => labels = true,
            "--no-labels" => no_labels = true,
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
    if bot && no_bot {
        return err("--bot and --no-bot conflict");
    }
    if labels && no_bot {
        return err("--labels needs the bot, which --no-bot turns off");
    }
    let bot = !no_bot;
    if labels && no_labels {
        return err("--labels and --no-labels conflict");
    }
    let implied = bot && !labels && !no_labels;
    let labels = labels || implied;
    let (mut ws, notes) = match target(root, flags, dir, ci, tools, &bench, check, bot, false) {
        Ok(t) => t,
        Err(e) => return err(&e),
    };
    ws.labels = labels;
    for note in &notes {
        println!("{note}");
    }
    let callers = other_callers(root);
    for caller in &callers {
        println!(
            "warn     {caller} calls soothfast; its jobs run twice until it or {WORKFLOW_PATH} is removed"
        );
    }
    let duplicate_only = ws.projects.is_empty() && ws.ci.is_default();
    let skip_workflow = skips_workflow(root, &callers, duplicate_only, force);
    if skip_workflow {
        println!(
            "init: {WORKFLOW_PATH} not written, since it would only duplicate the jobs above; --force writes it"
        );
    }
    let files: Vec<_> = scaffold(&ws)
        .into_iter()
        .filter(|f| !skip_workflow || f.kind == Kind::Bot)
        .collect();
    if files.is_empty() {
        return 0;
    }
    let rc = report(root, &files, check, force);
    if check || rc != 0 {
        return rc;
    }
    println!("init: done. Next: {}", next_step(ws.rust));
    if bot {
        println!(
            "init: the bot needs the App installed once (https://github.com/apps/soothfast-bot); the soothfast-bot environment is created on the first run. --no-bot skips both."
        );
    }
    if labels {
        return label_exit(create_labels(root), implied);
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

/// What the scaffold under `root` derives from, and a line per project
/// found.
#[allow(clippy::too_many_arguments)]
fn target(
    root: &Path,
    flags: Settings,
    dir: Option<String>,
    ci: Ci,
    tools: Pins,
    bench: &[String],
    check: bool,
    bot: bool,
    offline: bool,
) -> Result<(Workspace, Vec<String>), String> {
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
    let (projects, notes) = plan(root, stored, flags, dir)?;
    if !rust && projects.is_empty() {
        return Err(if notes.is_empty() {
            "no Cargo.toml, build.gradle(.kts), pom.xml, pyproject.toml, setup.py or \
             package.json found; pass --ecosystem"
                .into()
        } else {
            notes.join("\n")
        });
    }
    let markers = has_markers(root);
    let (benched, docs_packages) = if rust {
        invoke::workspace_root().map_err(|e| format!("not in a cargo workspace: {e}"))?;
        let meta = rust_meta(root, bench, check)?;
        let docs = if markers {
            docs_packages(root, &meta, &ci)
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
        default_branch: default_branch(root, &existing, offline),
        benched,
        docs_packages,
        action: action_ref(&existing, tools.action.as_deref(), offline),
        bot,
        labels: false,
        java: tools
            .java
            .or_else(|| pinned(&existing, "java"))
            .unwrap_or("21".into()),
        node: tools
            .node
            .or_else(|| pinned(&existing, "node"))
            .unwrap_or("22".into()),
    };
    Ok((ws, notes))
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

fn label(f: &Scaffold) -> String {
    match f.kind {
        Kind::Section => format!("{} [[project]]", f.path.display()),
        Kind::Runner => format!("{} [runner]", f.path.display()),
        Kind::Bot => format!("{} [bot]", f.path.display()),
        Kind::TestCi => format!("{} [test] ci", f.path.display()),
        Kind::Docs => format!("{} [docs] package", f.path.display()),
        _ => f.path.display().to_string(),
    }
}

fn skips_workflow(root: &Path, callers: &[String], duplicate_only: bool, force: bool) -> bool {
    !callers.is_empty() && duplicate_only && !force && !root.join(WORKFLOW_PATH).exists()
}

/// The files a flagless `init --check` reports, by path.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Drift {
    pub(crate) missing: Vec<String>,
    pub(crate) differs: Vec<String>,
}

/// What `init --check` finds, computed without the network and without
/// printing or writing.
pub(crate) fn drift(root: &Path) -> Result<Drift, String> {
    let (mut ws, _) = target(
        root,
        Settings::default(),
        None,
        Ci::default(),
        Pins::default(),
        &[],
        true,
        true,
        true,
    )?;
    ws.labels = true;
    let duplicate_only = ws.projects.is_empty() && ws.ci.is_default();
    let skip = skips_workflow(root, &other_callers(root), duplicate_only, false);
    let mut out = Drift::default();
    for f in scaffold(&ws)
        .iter()
        .filter(|f| !skip || f.kind == Kind::Bot)
    {
        let bucket = match f.state(read_text(root, &f.path)?.as_deref()) {
            State::Current => continue,
            State::Missing => &mut out.missing,
            State::Differs => &mut out.differs,
        };
        let path = f.path.display().to_string();
        if !bucket.contains(&path) {
            bucket.push(path);
        }
    }
    Ok(out)
}

fn report(root: &Path, files: &[Scaffold], check: bool, force: bool) -> i32 {
    let mut texts: BTreeMap<&Path, Result<Option<String>, String>> = BTreeMap::new();
    for f in files {
        texts
            .entry(&f.path)
            .or_insert_with(|| read_text(root, &f.path));
    }
    let current = |texts: &BTreeMap<&Path, Result<Option<String>, String>>, f: &Scaffold| {
        texts[f.path.as_path()].as_ref().ok().cloned()
    };
    let writing = !check
        && files
            .iter()
            .any(|f| current(&texts, f).is_some_and(|t| f.state(t.as_deref()) != State::Current));
    let mut drift = 0;
    let mut unreadable = BTreeSet::new();
    let mut drifted: Vec<&Path> = Vec::new();
    for f in files {
        let path = label(f);
        let Some(text) = current(&texts, f) else {
            if unreadable.insert(&f.path)
                && let Err(e) = &texts[f.path.as_path()]
            {
                eprintln!("error    {}: {e}; left as is", f.path.display());
            }
            continue;
        };
        let state = match f.state(text.as_deref()) {
            State::Current if writing && text.as_deref().is_some_and(|t| f.holds_legacy(t)) => {
                State::Differs
            }
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
                if state == State::Differs && !drifted.contains(&f.path.as_path()) {
                    drifted.push(&f.path);
                }
            }
            (State::Differs, false)
                if f.kind == Kind::Bot && text.as_deref().is_some_and(has_bot_table) =>
            {
                println!("warn     {path} has no `labels = true`, so the bot stays off; set it");
            }
            (State::Differs, false) if f.kind == Kind::Whole && !force => {
                drift += 1;
                println!("differs  {path} (left as is; --force rewrites it)");
            }
            (_, false) => match f.write(root, text.as_deref().unwrap_or_default()) {
                Ok(written) => {
                    println!("wrote    {path}");
                    texts.insert(&f.path, Ok(Some(written)));
                }
                Err(e) => return err(&format!("cannot write {path}: {e}")),
            },
        }
    }
    for path in drifted {
        let scaffolds: Vec<&Scaffold> = files.iter().filter(|f| f.path == path).collect();
        if let Ok(Some(text)) = &texts[path] {
            print!("{}", path_diff(&scaffolds, text));
        }
    }
    if check && drift > 0 {
        println!("init: {drift} file(s) drifted from `cargo soothfast init`; run it to update");
        return 1;
    }
    i32::from(drift > 0 || !unreadable.is_empty())
}

/// The runner labels and `[test] ci` setting of the repo at `root`.
pub fn ci(root: &Path) -> Result<Ci, String> {
    let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap_or_default();
    Ci::parse(&text).map_err(|e| format!("soothfast.toml: {e}"))
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
    use super::bot::BotLabels;
    use super::ecosystem::parse_projects_tolerant;
    use super::test_support::{mixed_ws, project, python_ws, scratch, ws};
    use super::*;

    const SHARED_TOML: &str = r##"[site]
name = "x"

[site.theme]
primary = "#336699"

[[site.nav]]
title = "Guide"
pages = ["index.md"]

[docs]
paths = ["README.md"]
package = "x"
coverage = 80

[[project]]
dir = "web"
ecosystem = "node"
future_key = "ignored"

[test]
ci = false

[gate]
codegen-units = 4

[changelog]
features = "a"

[package.x]
features = "all"

[package.x.gate]
features = "bench"
target = "perf"

[package.x.docs]
paths = ["book"]
coverage = 90

[[spec]]
path = "api.yaml"

[bot]
labels = true
merge_labels = ["ship"]

[[proto]]
proto = "wire.proto"
message = "Wire"
source = "src/wire.rs"
struct = "Wire"

[ci]
skip = ["sdk"]

[[sdk]]
spec = "api.yaml"
out = "sdk"
package = "x"

[[test.suite]]
label = "e2e"
project = "web"
cmd = "npm test"
"##;

    #[test]
    fn every_reader_ignores_the_tables_it_does_not_own() {
        assert_eq!(
            soothfast_site::config::parse(SHARED_TOML).unwrap().name,
            "x"
        );
        assert_eq!(
            crate::config::docs::parse(SHARED_TOML).unwrap().package,
            Some(vec!["x".into()])
        );
        assert_eq!(Ci::parse(SHARED_TOML).unwrap().test, Some(false));
        assert_eq!(parse_projects_tolerant(SHARED_TOML).unwrap().len(), 1);
        assert_eq!(parse_suites(SHARED_TOML).unwrap().len(), 1);
        assert_eq!(
            crate::config::gate::parse(SHARED_TOML)
                .unwrap()
                .codegen_units
                .as_deref(),
            Some("4")
        );
        assert!(crate::config::changelog::parse(SHARED_TOML).is_ok());
        assert_eq!(BotLabels::parse(SHARED_TOML).unwrap().merge, ["ship"]);
        assert_eq!(
            crate::spec_config::parse(SHARED_TOML)
                .unwrap()
                .entries
                .len(),
            1
        );
        assert_eq!(
            crate::sdk_config::parse(SHARED_TOML).unwrap().entries.len(),
            1
        );
        assert!(crate::test::config::parse(SHARED_TOML).is_ok());
        assert_eq!(
            crate::config::docs::parse(SHARED_TOML).unwrap().coverage,
            Some(80)
        );
        assert_eq!(crate::config::ci::parse(SHARED_TOML).unwrap(), ["sdk"]);
        assert_eq!(
            crate::spec_config::parse(SHARED_TOML).unwrap().protos.len(),
            1
        );
        assert_eq!(
            crate::config::package::parse(SHARED_TOML).unwrap()["x"]
                .base
                .features
                .as_deref(),
            Some("all")
        );
    }

    #[test]
    fn package_ci_proto_and_coverage_tables_are_not_drift() {
        let root = scratch("package-tables");
        let own = "[package.x]\nfeatures = \"all\"\n\n[package.x.gate]\nfeatures = \"bench\"\n\n[docs]\npackage = \"x\"\ncoverage = 80\n\n[ci]\nskip = [\"sdk\"]\n\n[[proto]]\nproto = \"w.proto\"\nmessage = \"W\"\nsource = \"w.rs\"\nstruct = \"W\"\n";
        std::fs::write(root.join("soothfast.toml"), own).unwrap();
        let mut files = scaffold(&ws(&[], Some("x")));
        files.retain(|f| f.path == Path::new("soothfast.toml"));
        assert!(files.len() > 1);
        assert_eq!(report(&root, &files, true, false), 0);
        assert_eq!(report(&root, &files, false, false), 0);
        assert_eq!(
            std::fs::read_to_string(root.join("soothfast.toml")).unwrap(),
            own
        );
    }

    #[test]
    fn labels_scaffold_a_bot_table_once_and_leave_an_existing_one() {
        let root = scratch("bot-table");
        let w = Workspace {
            labels: true,
            ..ws(&[], None)
        };
        let files = scaffold(&w);
        assert_eq!(report(&root, &files, true, false), 1);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert!(text.ends_with("\n\n[bot]\nlabels = true\n"));
        assert_eq!(report(&root, &files, true, false), 0);
        let own = "[bot]\nlabels = true\nmerge_labels = [\"ship\"]\n";
        std::fs::write(root.join("soothfast.toml"), own).unwrap();
        assert_eq!(report(&root, &files, true, false), 0);
        assert_eq!(report(&root, &files, false, false), 0);
        assert_eq!(
            std::fs::read_to_string(root.join("soothfast.toml")).unwrap(),
            own
        );
    }

    #[test]
    fn a_bot_table_without_labels_true_differs_and_is_not_rewritten() {
        let root = scratch("bot-off");
        let own = "[bot]\nmerge_labels = [\"ship\"]\n";
        std::fs::write(root.join("soothfast.toml"), own).unwrap();
        let w = Workspace {
            labels: true,
            ..ws(&[], None)
        };
        let files = scaffold(&w);
        assert_eq!(report(&root, &files, true, false), 1);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert_eq!(text, own);
    }

    #[test]
    fn the_bot_flags_conflict_with_each_other_and_with_labels() {
        assert_eq!(run(&["--bot".into(), "--no-bot".into()]), 2);
        assert_eq!(run(&["--labels".into(), "--no-bot".into()]), 2);
    }

    #[test]
    fn labels_and_no_labels_conflict() {
        assert_eq!(run(&["--labels".into(), "--no-labels".into()]), 2);
    }

    #[test]
    fn a_repo_with_a_caller_workflow_still_gets_the_bot_table() {
        let root = scratch("caller-bot");
        let dir = root.join(".github/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ci.yml"),
            "      - uses: Verdenroz/soothfast@abc\n",
        )
        .unwrap();
        let w = Workspace {
            bot: true,
            labels: true,
            ..ws(&[], None)
        };
        let mut files = scaffold(&w);
        files.retain(|f| f.kind == Kind::Bot);
        assert_eq!(files.len(), 1);
        assert_eq!(report(&root, &files, true, false), 1);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert_eq!(text, "[bot]\nlabels = true\n");
        assert!(!root.join(WORKFLOW_PATH).exists());
        assert!(!root.join(".gitignore").exists());
        assert_eq!(report(&root, &files, true, false), 0);
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
    fn a_file_that_is_not_text_is_reported_and_left_alone() {
        let root = scratch("not-utf8");
        std::fs::write(root.join(".gitignore"), [0xff, 0xfe, b'\n']).unwrap();
        let ignore = Scaffold {
            path: ".gitignore".into(),
            kind: Kind::Line,
            content: ".soothfast/".into(),
        };
        assert!(read_text(&root, &ignore.path).is_err());
        assert_eq!(report(&root, &[ignore], true, false), 1);
        let ignore = Scaffold {
            path: ".gitignore".into(),
            kind: Kind::Line,
            content: ".soothfast/".into(),
        };
        assert_eq!(report(&root, &[ignore], false, false), 1);
        assert_eq!(
            std::fs::read(root.join(".gitignore")).unwrap(),
            [0xff, 0xfe, b'\n']
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
