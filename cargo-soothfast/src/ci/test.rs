//! `ci test`: the workflow's test job, for the cargo workspace or, with
//! `--project DIR`, for one project outside it.

use std::process::Command;

use super::env::{CiEnv, Event};
use super::record::{result_path, run_recorded};

const WORKSPACE_NAME: &str = "test-rust";

/// What `cargo soothfast test` runs as, and the arguments that make it so.
#[derive(Debug, PartialEq)]
struct Job {
    name: String,
    args: Vec<String>,
}

/// Entry point for `ci test`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    let job = match assemble(env, args, base_exists) {
        Ok(job) => job,
        Err(e) => return crate::arg_err(&e),
    };
    let mut cmd = Command::new(exe);
    cmd.arg("test").args(&job.args);
    run_recorded(env, &job.name, "test", &mut cmd)
}

fn base_exists(base: &str) -> bool {
    Command::new("git")
        .args(["cat-file", "-e", &format!("{base}^{{commit}}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn assemble(
    env: &CiEnv,
    args: &[String],
    base_exists: impl Fn(&str) -> bool,
) -> Result<Job, String> {
    let mut project = None;
    let (mut suite, mut toolchain, mut name, mut all) = (None, None, None, false);
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match a.as_str() {
            "--project" => project = Some(value(a)?),
            "--suite" => suite = Some(value(a)?),
            "--toolchain" => toolchain = Some(value(a)?),
            "--name" => name = Some(value(a)?),
            "--all" => all = true,
            "--" => {
                rest.push(a.clone());
                rest.extend(it.by_ref().cloned());
            }
            _ => rest.push(a.clone()),
        }
    }
    if project.is_none() && name.is_some() {
        return Err("--name needs --project".into());
    }
    let job = match project {
        Some(dir) => {
            let name = name.unwrap_or_else(|| format!("test-{}", slug(&dir)));
            let mut args = vec!["--project".into(), dir, "--all".into(), "--no-cache".into()];
            args.extend(
                toolchain
                    .into_iter()
                    .flat_map(|t| ["--toolchain".into(), t]),
            );
            args.extend(suite.into_iter().flat_map(|s| ["--suite".into(), s]));
            args.extend(result_flags(env, &name));
            args.extend(["--name".into(), name.clone()]);
            Job { name, args }
        }
        None => {
            let mut args = selection(env, all, base_exists);
            args.extend(result_flags(env, WORKSPACE_NAME));
            Job {
                name: WORKSPACE_NAME.into(),
                args,
            }
        }
    };
    Ok(Job {
        args: [job.args, rest].concat(),
        ..job
    })
}

/// A pull request or push tests what its base says changed, and everything
/// when the base is unknown or not in the checkout. A local run with no base
/// leaves the choice to `test`.
fn selection(env: &CiEnv, all: bool, base_exists: impl Fn(&str) -> bool) -> Vec<String> {
    let flags = |list: &[&str]| list.iter().map(|s| s.to_string()).collect();
    match &env.base {
        _ if all => flags(&["--all", "--no-cache"]),
        Some(base) if base_exists(base) => vec!["--base".into(), base.clone()],
        None if env.event == Event::Other => Vec::new(),
        _ => flags(&["--all"]),
    }
}

fn result_flags(env: &CiEnv, name: &str) -> Vec<String> {
    match &env.results {
        Some(dir) => vec![
            "--result".into(),
            result_path(dir, name).display().to_string(),
        ],
        None => Vec::new(),
    }
}

fn slug(dir: &str) -> String {
    let slug: String = dir
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    slug.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::env::Flags;
    use std::collections::BTreeMap;

    fn env(event: Event, base: Option<&str>, results: Option<&str>) -> CiEnv {
        let flags = Flags {
            event: Some(event),
            base: base.map(String::from),
            results: results.map(Into::into),
        };
        CiEnv::parse(&BTreeMap::new(), None, &flags)
    }

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn present(_: &str) -> bool {
        true
    }

    #[test]
    fn a_pull_request_tests_from_its_base() {
        let env = env(Event::PullRequest, Some("b4se"), Some("out"));
        let job = assemble(&env, &[], present).unwrap();
        assert_eq!(job.name, "test-rust");
        assert_eq!(
            job.args,
            ["--base", "b4se", "--result", "out/test-rust.json"]
        );
    }

    #[test]
    fn a_push_without_a_usable_base_tests_everything() {
        let missing = |_: &str| false;
        let env_missing = env(Event::Push, Some("gone"), None);
        assert_eq!(
            assemble(&env_missing, &[], missing).unwrap().args,
            ["--all"]
        );
        let env_new = env(Event::Push, None, None);
        assert_eq!(assemble(&env_new, &[], present).unwrap().args, ["--all"]);
    }

    #[test]
    fn all_overrides_the_base_and_skips_stored_passes() {
        let env = env(Event::PullRequest, Some("b4se"), None);
        let job = assemble(&env, &argv(&["--all"]), present).unwrap();
        assert_eq!(job.args, ["--all", "--no-cache"]);
    }

    #[test]
    fn a_local_run_without_a_base_lets_test_choose() {
        let env = env(Event::Other, None, None);
        let job = assemble(&env, &argv(&["--no-doc", "--", "x"]), present).unwrap();
        assert_eq!(job.args, ["--no-doc", "--", "x"]);
    }

    #[test]
    fn a_project_runs_everything_under_its_own_name() {
        let env = env(Event::PullRequest, Some("b4se"), Some("out"));
        let args = argv(&[
            "--project",
            "web/app",
            "--suite",
            "unit",
            "--toolchain",
            "node-20",
        ]);
        let job = assemble(&env, &args, present).unwrap();
        assert_eq!(job.name, "test-web-app");
        assert_eq!(
            job.args,
            [
                "--project",
                "web/app",
                "--all",
                "--no-cache",
                "--toolchain",
                "node-20",
                "--suite",
                "unit",
                "--result",
                "out/test-web-app.json",
                "--name",
                "test-web-app",
            ]
        );
        let named = assemble(
            &env,
            &argv(&["--project", "a", "--name", "test-x"]),
            present,
        );
        assert_eq!(named.unwrap().name, "test-x");
    }

    #[test]
    fn a_flag_without_a_value_is_an_error() {
        let env = env(Event::Other, None, None);
        assert!(assemble(&env, &argv(&["--project"]), present).is_err());
        assert!(assemble(&env, &argv(&["--name", "x"]), present).is_err());
    }
}
