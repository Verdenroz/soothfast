//! `ci docs`: the workflow's docs job, which merges the gate legs' baselines,
//! picks what `docs check` compares against and whether it evaluates claims,
//! then checks.

use std::path::PathBuf;

use super::env::{CiEnv, Event};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::env::Flags;
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

    #[test]
    fn options_reject_what_they_do_not_know() {
        assert!(parse(&argv(&["--nope"])).is_err());
        assert!(parse(&argv(&["--expect-shards", "0"])).is_err());
        assert!(parse(&argv(&["--baselines"])).is_err());
        assert_eq!(opts(&[]).shards, 1);
    }
}
