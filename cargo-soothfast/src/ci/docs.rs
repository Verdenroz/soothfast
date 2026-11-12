//! `ci docs`: the workflow's docs job, which merges the gate legs' baselines,
//! picks what `docs check` compares against and whether it evaluates claims,
//! then checks.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::env::{CiEnv, Event};
use super::record::run_recorded;
use crate::{init, invoke};

/// What `ci docs` takes on the command line.
#[derive(Debug, Default, PartialEq)]
struct Options {
    packages: Vec<String>,
    baselines: Option<PathBuf>,
    shards: u64,
    plan: Option<PathBuf>,
    unrecorded: bool,
}

/// What the checkout and the repository's configuration say.
#[derive(Debug, Default)]
struct Situation {
    in_ci: bool,
    event_name: String,
    base_ref: Option<String>,
    has_parent: bool,
    gate_packages: Vec<String>,
    projects: bool,
    docs_packages: Vec<String>,
    baseline_files: Vec<PathBuf>,
    plan: Option<PathBuf>,
}

/// The commands a run makes. Each is the arguments `cargo soothfast` takes.
#[derive(Debug, PartialEq)]
struct Steps {
    merge: Option<Vec<String>>,
    check: Vec<String>,
    skip_reason: Option<&'static str>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut opts = Options {
        shards: 1,
        ..Options::default()
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "-p" | "--package" => opts.packages.push(value()?),
            "--baselines" => opts.baselines = Some(PathBuf::from(value()?)),
            "--expect-shards" => {
                opts.shards = value()?
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or("--expect-shards needs a shard count of at least 1")?;
            }
            "--plan" => opts.plan = Some(PathBuf::from(value()?)),
            "--unrecorded" => opts.unrecorded = true,
            other => return Err(format!("unknown ci docs argument {other:?}")),
        }
    }
    Ok(opts)
}

/// The cargo gate gates a pull request and records a default-branch push,
/// nothing else.
fn gated(env: &CiEnv) -> bool {
    env.event == Event::PullRequest || (env.event == Event::Push && env.default_branch)
}

/// A push records every item in one unsharded leg.
fn expected_shards(env: &CiEnv, shards: u64) -> u64 {
    if env.event == Event::PullRequest && shards > 1 {
        shards
    } else {
        1
    }
}

fn against_ref(env: &CiEnv, sit: &Situation) -> Option<String> {
    if let Some(base) = &sit.base_ref {
        return Some(format!("origin/{base}"));
    }
    if let (Event::PullRequest, Some(base)) = (env.event, &env.base) {
        return Some(base.clone());
    }
    let in_a_run = sit.in_ci || env.event != Event::Other;
    (in_a_run && sit.has_parent).then(|| "HEAD^".into())
}

fn skip_reason(sit: &Situation) -> Option<&'static str> {
    (sit.gate_packages.is_empty() && !sit.projects).then_some("no gate measures this repository")
}

fn assemble(env: &CiEnv, opts: &Options, sit: &Situation) -> Steps {
    let packages = if opts.packages.is_empty() {
        &sit.docs_packages
    } else {
        &opts.packages
    };
    let mut check: Vec<String> = ["docs", "check"].map(String::from).to_vec();
    check.extend(packages.iter().flat_map(|p| ["-p".into(), p.clone()]));
    check.extend(
        against_ref(env, sit)
            .into_iter()
            .flat_map(|r| ["--against-ref".into(), r]),
    );
    let skip_reason = skip_reason(sit);
    if skip_reason.is_some() {
        check.push("--skip-claims".into());
    }
    let merge = match (&opts.baselines, skip_reason) {
        (Some(_), None) => Some(merge_args(env, opts, sit)),
        _ => None,
    };
    Steps {
        merge,
        check,
        skip_reason,
    }
}

fn merge_args(env: &CiEnv, opts: &Options, sit: &Situation) -> Vec<String> {
    let mut args: Vec<String> = ["baseline", "merge", "--into", "head", "--expect"]
        .map(String::from)
        .to_vec();
    args.push(expected_shards(env, opts.shards).to_string());
    args.extend(["--packages".into(), sit.gate_packages.join(" ")]);
    if !gated(env) {
        args.extend(["--not-gated".into(), sit.event_name.clone()]);
    }
    if let Some(plan) = &sit.plan {
        args.extend(["--plan".into(), plan.display().to_string()]);
    }
    args.extend(sit.baseline_files.iter().map(|f| f.display().to_string()));
    args
}

/// Runs the steps through `run`, which takes a command's arguments and
/// returns its exit code. A merge that fails still leaves binds and examples
/// to check, so the check runs without claims and the merge's code wins.
fn execute(steps: &Steps, mut run: impl FnMut(&[String]) -> i32) -> i32 {
    if let Some(reason) = steps.skip_reason {
        println!("claims not evaluated: {reason}");
    }
    let mut check = steps.check.clone();
    let mut merged = 0;
    if let Some(merge) = &steps.merge {
        merged = run(merge);
        let claims: &[&str] = if merged == 0 {
            &["--baseline", "head"]
        } else {
            &["--skip-claims"]
        };
        check.extend(claims.iter().map(|a| a.to_string()));
    }
    let checked = run(&check);
    if merged != 0 { merged } else { checked }
}

/// Entry point for `ci docs`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let opts = match parse(args) {
        Ok(opts) => opts,
        Err(e) => return crate::arg_err(&e),
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    if !opts.unrecorded {
        let mut cmd = Command::new(exe);
        cmd.args(["ci", "--event", event_name(env.event)]);
        cmd.args(env.base.iter().flat_map(|b| ["--base", b.as_str()]));
        cmd.arg("docs").args(args).arg("--unrecorded");
        return run_recorded(env, "docs", "docs", &mut cmd);
    }
    match situation(env, &opts) {
        Ok(sit) => execute(&assemble(env, &opts, &sit), |args| spawn(&exe, args)),
        Err(e) => {
            eprintln!("soothfast: {e}");
            2
        }
    }
}

fn spawn(exe: &Path, args: &[String]) -> i32 {
    match Command::new(exe).args(args).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("soothfast: {e}");
            2
        }
    }
}

fn event_name(event: Event) -> &'static str {
    match event {
        Event::PullRequest => "pull_request",
        Event::Push => "push",
        Event::Other => "other",
    }
}

fn situation(env: &CiEnv, opts: &Options) -> Result<Situation, String> {
    let root = invoke::project_root();
    let rust = root
        .join("Cargo.toml")
        .is_file()
        .then(|| init::rust_plan(&root))
        .transpose()?;
    let projects = init::manifest(&root)?.iter().any(|p| p.bench.is_some());
    let (gate_packages, docs_packages) =
        rust.map_or_else(Default::default, |r| (r.packages, r.docs_packages));
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let has_parent = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "HEAD^"])
        .output()
        .is_ok_and(|o| o.status.success());
    Ok(Situation {
        in_ci: var("GITHUB_ACTIONS").is_some(),
        event_name: var("GITHUB_EVENT_NAME").unwrap_or_else(|| event_name(env.event).into()),
        base_ref: var("GITHUB_BASE_REF"),
        has_parent,
        gate_packages,
        projects,
        docs_packages,
        baseline_files: opts
            .baselines
            .as_deref()
            .map(baseline_files)
            .unwrap_or_default(),
        plan: opts.plan.clone().filter(|p| p.is_file()),
    })
}

/// The `*.json` files in `dir` and one directory down. Several downloaded
/// artifacts each land in a directory of their own, and a lone one lands in
/// `dir` itself.
fn baseline_files(dir: &Path) -> Vec<PathBuf> {
    let entries = |d: &Path| -> Vec<PathBuf> {
        let listed = fs::read_dir(d).into_iter().flatten().flatten();
        listed.map(|e| e.path()).collect()
    };
    let (subdirs, top): (Vec<PathBuf>, Vec<PathBuf>) =
        entries(dir).into_iter().partition(|p| p.is_dir());
    let mut files: Vec<PathBuf> = subdirs
        .iter()
        .flat_map(|d| entries(d))
        .chain(top)
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::env::Flags;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    fn env(event: Event, base: Option<&str>, default_branch: bool) -> CiEnv {
        let flags = Flags {
            event: Some(event),
            base: base.map(String::from),
            results: None,
        };
        CiEnv {
            default_branch,
            ..CiEnv::parse(&BTreeMap::new(), None, &flags)
        }
    }

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn ci(event_name: &str) -> Situation {
        Situation {
            in_ci: true,
            event_name: event_name.into(),
            has_parent: true,
            gate_packages: argv(&["core", "cli"]),
            docs_packages: argv(&["core"]),
            ..Situation::default()
        }
    }

    fn opts(list: &[&str]) -> Options {
        parse(&argv(list)).unwrap()
    }

    #[test]
    fn a_pull_request_compares_against_its_base_branch() {
        let env = env(Event::PullRequest, Some("b4se"), false);
        let sit = Situation {
            base_ref: Some("main".into()),
            ..ci("pull_request")
        };
        let steps = assemble(&env, &opts(&[]), &sit);
        assert_eq!(
            steps.check,
            [
                "docs",
                "check",
                "-p",
                "core",
                "--against-ref",
                "origin/main"
            ]
        );
    }

    #[test]
    fn a_push_compares_against_its_parent() {
        let env = env(Event::Push, Some("pr3v"), true);
        let steps = assemble(&env, &opts(&["-p", "a", "-p", "b"]), &ci("push"));
        assert_eq!(
            steps.check,
            [
                "docs",
                "check",
                "-p",
                "a",
                "-p",
                "b",
                "--against-ref",
                "HEAD^"
            ]
        );
    }

    #[test]
    fn a_run_with_no_parent_compares_against_nothing() {
        let env = env(Event::Push, None, true);
        let sit = Situation {
            has_parent: false,
            ..ci("push")
        };
        let steps = assemble(&env, &opts(&[]), &sit);
        assert_eq!(steps.check, ["docs", "check", "-p", "core"]);
    }

    #[test]
    fn a_pull_request_given_a_base_commit_and_no_branch_uses_the_commit() {
        let env = env(Event::PullRequest, Some("b4se"), false);
        let sit = Situation {
            in_ci: false,
            ..ci("pull_request")
        };
        assert_eq!(against_ref(&env, &sit).as_deref(), Some("b4se"));
    }

    #[test]
    fn another_event_in_a_run_compares_against_its_parent() {
        let env = env(Event::Other, None, false);
        let sit = ci("workflow_dispatch");
        assert_eq!(against_ref(&env, &sit).as_deref(), Some("HEAD^"));
    }

    #[test]
    fn a_local_run_compares_against_nothing_and_checks_the_configured_packages() {
        let env = env(Event::Other, None, false);
        let sit = Situation {
            in_ci: false,
            ..ci("")
        };
        let steps = assemble(&env, &opts(&[]), &sit);
        assert_eq!(
            steps,
            Steps {
                merge: None,
                check: argv(&["docs", "check", "-p", "core"]),
                skip_reason: None,
            }
        );
    }

    #[test]
    fn claims_are_skipped_when_no_gate_measures_the_repository() {
        let env = env(Event::PullRequest, Some("b4se"), false);
        let sit = Situation {
            gate_packages: Vec::new(),
            ..ci("pull_request")
        };
        let steps = assemble(&env, &opts(&["--baselines", "dl"]), &sit);
        assert_eq!(steps.skip_reason, Some("no gate measures this repository"));
        assert_eq!(steps.merge, None);
        assert_eq!(
            steps.check.last().map(String::as_str),
            Some("--skip-claims")
        );
    }

    #[test]
    fn a_project_gate_is_a_gate_that_measures() {
        let sit = Situation {
            gate_packages: Vec::new(),
            projects: true,
            ..ci("push")
        };
        assert_eq!(skip_reason(&sit), None);
        assert_eq!(skip_reason(&ci("push")), None);
    }

    #[test]
    fn claims_are_evaluated_when_a_gate_measures() {
        let env = env(Event::Push, Some("pr3v"), true);
        let steps = assemble(&env, &opts(&[]), &ci("push"));
        assert!(!steps.check.contains(&"--skip-claims".to_string()));
    }

    #[test]
    fn baselines_given_are_merged_with_the_shard_count_a_pull_request_gates_with() {
        let env = env(Event::PullRequest, Some("b4se"), false);
        let sit = Situation {
            baseline_files: vec!["dl/a/x.json".into(), "dl/b/y.json".into()],
            plan: Some("plan/plan.json".into()),
            ..ci("pull_request")
        };
        let args = opts(&["--baselines", "dl", "--expect-shards", "3"]);
        let steps = assemble(&env, &args, &sit);
        assert_eq!(
            steps.merge.unwrap(),
            [
                "baseline",
                "merge",
                "--into",
                "head",
                "--expect",
                "3",
                "--packages",
                "core cli",
                "--plan",
                "plan/plan.json",
                "dl/a/x.json",
                "dl/b/y.json",
            ]
        );
    }

    #[test]
    fn a_push_expects_one_leg_whatever_the_shard_count() {
        let push = env(Event::Push, Some("pr3v"), true);
        let pull = env(Event::PullRequest, None, false);
        assert_eq!(expected_shards(&push, 4), 1);
        assert_eq!(expected_shards(&pull, 4), 4);
        assert_eq!(expected_shards(&pull, 1), 1);
    }

    #[test]
    fn a_run_the_gate_does_not_measure_on_merges_as_not_gated() {
        let on_branch = env(Event::Push, Some("pr3v"), false);
        let dispatch = env(Event::Other, None, false);
        let sit = ci("workflow_dispatch");
        let flags = opts(&["--baselines", "dl"]);
        for env in [&on_branch, &dispatch] {
            let merge = assemble(env, &flags, &sit).merge.unwrap();
            let at = merge.iter().position(|a| a == "--not-gated").unwrap();
            assert_eq!(merge[at + 1], "workflow_dispatch");
        }
        let gated_push = env(Event::Push, Some("pr3v"), true);
        let merge = assemble(&gated_push, &flags, &ci("push")).merge.unwrap();
        assert!(!merge.contains(&"--not-gated".to_string()));
    }

    #[test]
    fn no_baselines_means_no_merge() {
        let env = env(Event::Push, Some("pr3v"), true);
        assert_eq!(assemble(&env, &opts(&[]), &ci("push")).merge, None);
    }

    fn steps(merge: bool) -> Steps {
        Steps {
            merge: merge.then(|| argv(&["baseline", "merge"])),
            check: argv(&["docs", "check"]),
            skip_reason: None,
        }
    }

    fn run_with(steps: &Steps, codes: &[i32]) -> (i32, Vec<Vec<String>>) {
        let ran = RefCell::new(Vec::new());
        let mut codes = codes.iter();
        let code = execute(steps, |args| {
            ran.borrow_mut().push(args.to_vec());
            *codes.next().unwrap()
        });
        (code, ran.into_inner())
    }

    #[test]
    fn a_merged_baseline_is_what_the_check_reads() {
        let (code, ran) = run_with(&steps(true), &[0, 1]);
        assert_eq!(code, 1);
        assert_eq!(ran[0], ["baseline", "merge"]);
        assert_eq!(ran[1], ["docs", "check", "--baseline", "head"]);
    }

    #[test]
    fn a_failed_merge_checks_without_claims_and_keeps_its_code() {
        let (code, ran) = run_with(&steps(true), &[3, 0]);
        assert_eq!(code, 3);
        assert_eq!(ran[1], ["docs", "check", "--skip-claims"]);
    }

    #[test]
    fn without_a_merge_the_check_is_the_only_command() {
        let (code, ran) = run_with(&steps(false), &[1]);
        assert_eq!(code, 1);
        assert_eq!(ran, [["docs", "check"]]);
    }

    #[test]
    fn baselines_are_the_json_files_in_the_directory_and_one_down() {
        let dir = std::env::temp_dir().join(format!("soothfast-ci-docs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for leg in ["b", "a"] {
            fs::create_dir_all(dir.join(leg)).unwrap();
            fs::write(dir.join(leg).join("x.json"), "{}").unwrap();
        }
        fs::write(dir.join("a").join("note.txt"), "").unwrap();
        fs::write(dir.join("top.json"), "{}").unwrap();
        assert_eq!(
            baseline_files(&dir),
            [
                dir.join("a").join("x.json"),
                dir.join("b").join("x.json"),
                dir.join("top.json")
            ]
        );
        assert!(baseline_files(&dir.join("missing")).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn options_reject_what_they_do_not_know() {
        assert!(parse(&argv(&["--nope"])).is_err());
        assert!(parse(&argv(&["--expect-shards", "0"])).is_err());
        assert!(parse(&argv(&["--baselines"])).is_err());
        assert_eq!(opts(&[]).shards, 1);
    }
}
