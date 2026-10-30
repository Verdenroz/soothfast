//! `cargo soothfast test --project DIR`: run the tests of a project that is
//! not part of the cargo workspace, when a change reaches it and no stored
//! pass covers the same files, command and toolchain.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use serde_json::json;

use super::key::{Passes, file_ids};
use super::select;
use crate::init::ecosystem::{Ecosystem, Project, Suite, normalize_dir};
use crate::runcache::hashed;
use crate::{glob, init, invoke};

/// The label a writer applies to ignore selection and stored passes.
pub const TEST_ALL_LABEL: &str = "soothfast:test-all";

/// Lines of output kept for the result.
const TAIL: usize = 40;

#[derive(Default, Debug, PartialEq)]
struct Args {
    project: String,
    suite: Option<String>,
    base: Option<String>,
    all: bool,
    no_cache: bool,
    toolchain: Option<String>,
    result: Option<PathBuf>,
    name: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut project = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (a.as_str(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--project" => project = Some(value()?),
            "--suite" => out.suite = Some(value()?),
            "--base" => out.base = Some(value()?),
            "--toolchain" => out.toolchain = Some(value()?),
            "--result" => out.result = Some(PathBuf::from(value()?)),
            "--name" => out.name = Some(value()?),
            "--all" => out.all = true,
            "--no-cache" => out.no_cache = true,
            other => return Err(format!("unknown flag {other:?} with --project")),
        }
    }
    out.project = project.ok_or("--project needs a value")?;
    Ok(out)
}

/// How a project's run ended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verdict {
    Pass,
    Reused,
    Flaky,
    Failed,
    Skipped,
}

impl Verdict {
    pub fn name(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Reused => "reused",
            Verdict::Flaky => "flaky",
            Verdict::Failed => "failed",
            Verdict::Skipped => "skipped",
        }
    }
}

/// What a run executes.
pub struct Target {
    ecosystem: Ecosystem,
    dir: String,
    cmd: String,
    watch: Vec<String>,
    retries: u32,
    retry_cmd: Option<String>,
}

/// The toolchain versions a workflow installs, which name the toolchain in a
/// key without probing a binary that may not be installed yet.
#[derive(Default, Debug, PartialEq)]
pub struct Pins {
    pub java: Option<String>,
    pub node: Option<String>,
    pub runner: Option<String>,
}

pub fn run(args: &[String]) -> i32 {
    match parse_args(args).and_then(drive) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: {e}");
            2
        }
    }
}

fn drive(args: Args) -> Result<i32, String> {
    let root = invoke::project_root();
    let target = target(&root, &args)?;
    let started = Instant::now();
    let finish = |verdict: Verdict, output: Vec<String>| -> Result<i32, String> {
        if let Some(path) = &args.result {
            let name = args.name.as_deref().unwrap_or(&args.project);
            let wall_s = started.elapsed().as_secs_f64();
            write_result(path, name, &args.project, verdict, &output, wall_s)?;
        }
        Ok(i32::from(verdict == Verdict::Failed))
    };

    let selected = args.all
        || args.suite.is_some()
        || reaches(&root, args.base.as_deref(), &target).map_err(|e| e.to_string())?;
    if !selected {
        eprintln!("test: {} is untouched by this change; skipped", target.dir);
        return finish(Verdict::Skipped, Vec::new());
    }
    let toolchain = args
        .toolchain
        .clone()
        .unwrap_or_else(|| toolchain(target.ecosystem));
    let files = Files::read(&root).map_err(|e| e.to_string())?;
    let key = cache_key(&files, &target, &toolchain);
    let passes = Passes::open();
    if !args.no_cache && passes.as_ref().is_some_and(|p| p.has(&key)) {
        eprintln!("test: {} reused a stored pass", target.dir);
        return finish(Verdict::Reused, Vec::new());
    }
    let (verdict, output) = execute_with_retries(&root, &target);
    if let (Verdict::Pass, Some(passes)) = (verdict, &passes) {
        passes.record(&target.dir, &key);
    }
    finish(verdict, output)
}

fn target(root: &Path, args: &Args) -> Result<Target, String> {
    let dir = normalize_dir(&args.project)?;
    let project = init::manifest(root)?
        .into_iter()
        .find(|p| p.dir == dir)
        .ok_or_else(|| format!("no [[project]] at {dir} in soothfast.toml"))?;
    let suite = match &args.suite {
        None => None,
        Some(label) => Some(
            init::suites(root)?
                .into_iter()
                .find(|s| &s.label == label && s.project == dir)
                .ok_or_else(|| format!("no [[test.suite]] labelled {label:?} for {dir}"))?,
        ),
    };
    target_for(&project, suite.as_ref())
}

/// What `project`'s tests run, or `suite`'s command in the project's
/// directory. A suite reruns its own command, not the project's `retry_cmd`.
pub fn target_for(project: &Project, suite: Option<&Suite>) -> Result<Target, String> {
    let (cmd, retry_cmd) = match suite {
        None => (
            project
                .test
                .cmd
                .clone()
                .ok_or_else(|| format!("{} has no test_cmd", project.dir))?,
            project.test.retry_cmd.clone(),
        ),
        Some(suite) => (suite.cmd.clone(), None),
    };
    Ok(Target {
        ecosystem: project.ecosystem,
        dir: project.dir.clone(),
        cmd,
        watch: project.test.watch.clone(),
        retries: project.test.retries,
        retry_cmd,
    })
}

/// The suites whose label a writer has applied.
pub fn suites_for<'a>(suites: &'a [Suite], labels: &[String]) -> Vec<&'a Suite> {
    suites
        .iter()
        .filter(|s| labels.contains(&s.label))
        .collect()
}

/// Whether a change to any of `changed` (relative to the repository root)
/// runs the project at `dir`.
pub fn touches(dir: &str, watch: &[String], changed: &BTreeSet<String>) -> bool {
    changed
        .iter()
        .any(|f| within(dir, f) || watch.iter().any(|g| glob::matches(g, f)))
}

fn within(dir: &str, file: &str) -> bool {
    dir == "."
        || file
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The files changed since `base`'s merge-base, or none when the base shares
/// no history with HEAD, in which case everything counts as changed.
pub fn changed_since(root: &Path, base: Option<&str>) -> io::Result<Option<BTreeSet<String>>> {
    let base = match base {
        Some(base) => base.to_string(),
        None => select::default_base(root)?,
    };
    if !select::has_merge_base(root, &base)? {
        return Ok(None);
    }
    select::changed_files(root, &base).map(Some)
}

fn reaches(root: &Path, base: Option<&str>, target: &Target) -> io::Result<bool> {
    Ok(changed_since(root, base)?
        .is_none_or(|changed| touches(&target.dir, &target.watch, &changed)))
}

/// Every file of the repository by blob id, tracked or not: read once and
/// shared by the keys of all the projects.
pub struct Files(BTreeMap<String, String>);

impl Files {
    pub fn read(root: &Path) -> io::Result<Files> {
        file_ids(root, ".", &|_| true).map(Files)
    }
}

/// The key a pass is stored under: the content of every file in the project
/// or matched by its watch globs, the command, and the toolchain.
fn cache_key(files: &Files, target: &Target, toolchain: &str) -> String {
    let mut parts: Vec<String> = files
        .0
        .iter()
        .filter(|(f, _)| within(&target.dir, f) || target.watch.iter().any(|g| glob::matches(g, f)))
        .map(|(f, id)| format!("{f}\u{0}{id}"))
        .collect();
    parts.push(target.cmd.clone());
    parts.push(toolchain.to_string());
    hashed(&parts)
}

/// The names among `tests` (name, target, toolchain) that a stored pass
/// covers.
pub fn covered(root: &Path, tests: &[(String, Target, String)]) -> io::Result<BTreeSet<String>> {
    let Some(passes) = Passes::open() else {
        return Ok(BTreeSet::new());
    };
    let files = Files::read(root)?;
    Ok(tests
        .iter()
        .filter(|(_, target, toolchain)| passes.has(&cache_key(&files, target, toolchain)))
        .map(|(name, ..)| name.clone())
        .collect())
}

/// The toolchain `eco` runs on: named by `pins` when the runner is known,
/// else whatever is installed.
pub fn toolchain_for(eco: Ecosystem, pins: &Pins) -> String {
    let Some(runner) = &pins.runner else {
        return toolchain(eco);
    };
    let version = match eco {
        Ecosystem::Node => pins.node.as_deref(),
        Ecosystem::Python => Some("uv"),
        Ecosystem::Rust => None,
        Ecosystem::Gradle | Ecosystem::Maven => pins.java.as_deref(),
    };
    format!("{} {} {runner}", eco.name(), version.unwrap_or_default())
}

/// What `eco`'s toolchain reports as its version, empty when it is absent.
fn toolchain(eco: Ecosystem) -> String {
    let argv: &[&str] = match eco {
        Ecosystem::Rust => &["rustc", "--version"],
        Ecosystem::Gradle | Ecosystem::Maven => &["java", "-version"],
        Ecosystem::Python => &["python", "--version"],
        Ecosystem::Node => &["node", "--version"],
    };
    // `java -version` prints to stderr.
    Command::new(argv[0])
        .args(&argv[1..])
        .output()
        .map(|o| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
            .trim()
            .to_string()
        })
        .unwrap_or_default()
}

/// Run `attempt` once, then again up to `retries` times while it fails. It is
/// told whether it is a retry. A pass after a failure is flaky.
pub fn with_retries(retries: u32, mut attempt: impl FnMut(bool) -> bool) -> Verdict {
    if attempt(false) {
        return Verdict::Pass;
    }
    if (0..retries).any(|_| attempt(true)) {
        Verdict::Flaky
    } else {
        Verdict::Failed
    }
}

fn execute_with_retries(root: &Path, target: &Target) -> (Verdict, Vec<String>) {
    let dir = root.join(&target.dir);
    let mut output = Vec::new();
    let verdict = with_retries(target.retries, |retry| {
        let cmd = match (&target.retry_cmd, retry) {
            (Some(retry_cmd), true) => retry_cmd,
            _ => &target.cmd,
        };
        if retry {
            println!("test: {} failed, retrying with `{cmd}`", target.dir);
        }
        let (passed, tail) = execute(&dir, cmd);
        output = tail;
        passed
    });
    if verdict == Verdict::Flaky {
        println!("test: {} passed on a retry; flaky", target.dir);
    }
    (verdict, output)
}

/// Run `cmd` in `dir` through the shell, echoing its output and keeping the
/// tail. A command that cannot start fails with the reason as its output.
fn execute(dir: &Path, cmd: &str) -> (bool, Vec<String>) {
    let spawned = Command::new("sh")
        .args(["-c", &format!("exec 2>&1\n{cmd}")])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => return (false, vec![format!("cannot run `{cmd}`: {e}")]),
    };
    let mut tail = VecDeque::new();
    if let Some(stdout) = child.stdout.take() {
        let mut out = BufReader::new(stdout);
        let mut raw = Vec::new();
        while matches!(out.read_until(b'\n', &mut raw), Ok(n) if n > 0) {
            let line = String::from_utf8_lossy(&raw).trim_end().to_string();
            raw.clear();
            println!("{line}");
            if tail.len() == TAIL {
                tail.pop_front();
            }
            tail.push_back(line);
        }
    }
    let passed = child.wait().is_ok_and(|s| s.success());
    (passed, tail.into())
}

/// Write the result document of one project's tests.
pub fn write_result(
    path: &Path,
    name: &str,
    project: &str,
    verdict: Verdict,
    output: &[String],
    wall_s: f64,
) -> Result<(), String> {
    let doc = json!({
        "name": name,
        "project": project,
        "kind": "test",
        "verdict": verdict.name(),
        "output": output.join("\n"),
        "wall_s": wall_s,
    });
    std::fs::write(path, format!("{doc}\n")).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::fixture::TempRepo;

    fn files(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|f| f.to_string()).collect()
    }

    fn target_at(dir: &str, watch: &[&str], cmd: &str) -> Target {
        Target {
            ecosystem: Ecosystem::Node,
            dir: dir.into(),
            cmd: cmd.into(),
            watch: watch.iter().map(|g| g.to_string()).collect(),
            retries: 0,
            retry_cmd: None,
        }
    }

    #[test]
    fn a_change_under_the_directory_or_a_watch_glob_runs_the_project() {
        let watch = [
            "gradle/libs.versions.toml".to_string(),
            "shared/**".to_string(),
        ];
        assert!(touches("client", &[], &files(&["client/src/A.java"])));
        assert!(touches(
            "client",
            &watch,
            &files(&["gradle/libs.versions.toml"])
        ));
        assert!(touches("client", &watch, &files(&["shared/a/b.txt"])));
        assert!(!touches(
            "client",
            &watch,
            &files(&["server/a.rs", "clients/x"])
        ));
        assert!(touches(".", &[], &files(&["anything"])));
        assert!(!touches("client", &[], &BTreeSet::new()));
    }

    #[test]
    fn selection_follows_the_diff_against_the_base() {
        let repo = TempRepo::new("proj-select");
        repo.write("web/a.js", "1");
        repo.write("api/b.py", "1");
        repo.write("gradle/libs.versions.toml", "1");
        repo.git(&["add", "."]);
        repo.commit();
        repo.write("api/b.py", "2");
        repo.git(&["add", "."]);
        repo.commit();
        let web = target_at("web", &["gradle/libs.versions.toml"], "npm test");
        assert!(!reaches(&repo.root, Some("HEAD~1"), &web).unwrap());
        assert!(reaches(&repo.root, Some("HEAD~1"), &target_at("api", &[], "x")).unwrap());
        repo.write("gradle/libs.versions.toml", "2");
        repo.git(&["add", "."]);
        repo.commit();
        assert!(reaches(&repo.root, Some("HEAD~1"), &web).unwrap());
    }

    #[test]
    fn the_key_moves_with_files_command_and_toolchain() {
        let repo = TempRepo::new("proj-key");
        repo.write("web/a.js", "1");
        repo.write("other/b.js", "1");
        repo.git(&["add", "."]);
        let web = target_at("web", &[], "npm test");
        let key = |t: &Target, tc: &str| cache_key(&Files::read(&repo.root).unwrap(), t, tc);
        let first = key(&web, "node v22");
        assert_eq!(first, key(&web, "node v22"));
        assert_ne!(first, key(&web, "node v24"));
        assert_ne!(first, key(&target_at("web", &[], "npm run t"), "node v22"));
        repo.write("other/b.js", "2");
        assert_eq!(first, key(&web, "node v22"));
        repo.write("web/a.js", "2");
        assert_ne!(first, key(&web, "node v22"));
        let watched = target_at("web", &["other/*"], "npm test");
        let before = key(&watched, "node v22");
        repo.write("other/b.js", "3");
        assert_ne!(before, key(&watched, "node v22"));
    }

    #[test]
    fn untracked_and_dirty_files_move_the_key() {
        let repo = TempRepo::new("proj-dirty");
        repo.write("web/a.js", "1");
        repo.git(&["add", "."]);
        repo.commit();
        let web = target_at("web", &[], "npm test");
        let key = || cache_key(&Files::read(&repo.root).unwrap(), &web, "t");
        let clean = key();
        repo.write("web/new.js", "x");
        let untracked = key();
        assert_ne!(clean, untracked);
        repo.git(&["add", "."]);
        assert_eq!(untracked, key());
        repo.write("web/a.js", "2");
        assert_ne!(untracked, key());
    }

    #[test]
    fn a_pass_after_a_failure_is_flaky_and_exhausted_retries_fail() {
        let mut calls = Vec::new();
        let verdict = with_retries(2, |retry| {
            calls.push(retry);
            calls.len() == 2
        });
        assert_eq!(verdict, Verdict::Flaky);
        assert_eq!(calls, [false, true]);
        assert_eq!(with_retries(2, |_| false), Verdict::Failed);
        assert_eq!(with_retries(0, |_| false), Verdict::Failed);
        assert_eq!(with_retries(3, |_| true), Verdict::Pass);
    }

    #[test]
    fn a_command_that_fails_once_is_flaky_and_its_retry_command_is_used() {
        let dir = std::env::temp_dir().join(format!("soothfast-flaky-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut target = target_at(
            ".",
            &[],
            "test -f seen || { touch seen; echo boom; exit 1; }",
        );
        target.retries = 1;
        target.retry_cmd = Some("echo retried".into());
        let (verdict, output) = execute_with_retries(&dir, &target);
        assert_eq!(verdict, Verdict::Flaky);
        assert_eq!(output, ["retried"]);
        let (verdict, output) = execute(&dir, "echo one; echo two >&2; exit 3");
        assert!(!verdict);
        assert_eq!(output, ["one", "two"]);
        let (verdict, output) = execute(&dir, "echo ok # trailing comment");
        assert!(verdict);
        assert_eq!(output, ["ok"]);
        let (verdict, output) = execute(&dir, "printf 'a\\377b\\nc\\n'");
        assert!(verdict);
        assert_eq!(output, ["a\u{fffd}b", "c"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_labelled_suites_run() {
        let suite = |label: &str| Suite {
            label: label.into(),
            project: "web".into(),
            cmd: "npm run e2e".into(),
        };
        let suites = [suite("soothfast:e2e"), suite("soothfast:slow")];
        let picked = suites_for(&suites, &["soothfast:slow".into(), "other".into()]);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].label, "soothfast:slow");
        assert!(suites_for(&suites, &[]).is_empty());
    }

    #[test]
    fn flags_are_parsed_and_unknown_ones_rejected() {
        let a = parse_args(&["--project=web".into(), "--base".into(), "b".into()]).unwrap();
        assert_eq!((a.project.as_str(), a.base.as_deref()), ("web", Some("b")));
        assert!(parse_args(&["--project".into(), "web".into(), "--nope".into()]).is_err());
        assert!(parse_args(&["--all".into()]).is_err());
    }
}
