//! `ci site`: builds the docs site when `soothfast.toml` configures one, so a
//! change that breaks the build fails before it is published.

use std::path::Path;
use std::process::Command;

use soothfast_site::toml::logical_lines;

use super::each::{self, Job};
use super::env::CiEnv;
use crate::config::Config;
use crate::invoke;

const DEFAULT_OUT: &str = "target/soothfast-site";

/// `--out DIR`, and whether this is the inner run a recorded job makes.
fn parse(args: &[String]) -> Result<(String, bool), String> {
    let mut out = DEFAULT_OUT.to_string();
    let mut inner = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = it.next().cloned().ok_or("--out needs a value")?,
            "--inner" => inner = true,
            other => return Err(format!("unknown ci site argument {other:?}")),
        }
    }
    Ok((out, inner))
}

/// Whether the text has a `[site]` table or one of its sub-tables.
fn has_site(text: &str) -> bool {
    logical_lines(text).iter().any(|(_, line)| {
        let table = line.trim_start_matches('[');
        table.starts_with("site]") || table.starts_with("site.")
    })
}

fn command(out: &str, head_baseline: bool) -> Vec<String> {
    let mut args: Vec<String> = ["docs", "build", "--out", out].map(String::from).to_vec();
    if head_baseline {
        args.extend(["--baseline".into(), "head".into()]);
    }
    args
}

/// Whether the repository configures a site to build.
pub(super) fn configured() -> Result<bool, String> {
    Ok(job(DEFAULT_OUT)?.is_some())
}

/// What the recorded job runs: the build alone, or this kind again with
/// `--inner` when a `site_run` hook has to run first under the same record.
fn job_args(out: &str, head_baseline: bool, hook: bool) -> Vec<String> {
    if hook {
        return ["ci", "site", "--out", out, "--inner"]
            .map(String::from)
            .to_vec();
    }
    command(out, head_baseline)
}

fn head_baseline(root: &Path) -> bool {
    invoke::baseline_path(root, "head").is_file()
}

fn job(out: &str) -> Result<Option<Job>, String> {
    let root = invoke::workspace_root().map_err(|e| e.to_string())?;
    let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap_or_default();
    if !has_site(&text) {
        return Ok(None);
    }
    let hook = Config::load(&root)?.site_run()?.is_some();
    Ok(Some(Job {
        name: "site".into(),
        args: job_args(out, head_baseline(&root), hook),
    }))
}

/// Runs the `site_run` hook with `$SOOTHFAST` naming this CLI and
/// `$SOOTHFAST_BASELINE` the baseline the build reads, then builds. A hook
/// that fails stops the build.
fn inner(out: &str) -> Result<i32, String> {
    let root = invoke::workspace_root().map_err(|e| e.to_string())?;
    let head = head_baseline(&root);
    if let Some(script) = Config::load(&root)?.site_run()? {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let status = Command::new("bash")
            .args(["-euo", "pipefail", "-c", &script])
            .env("SOOTHFAST", exe)
            .env("SOOTHFAST_BASELINE", if head { "head" } else { "" })
            .current_dir(&root)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Ok(status.code().unwrap_or(1));
        }
    }
    Ok(each::spawn(&command(out, head)))
}

/// Entry point for `ci site`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let (out, is_inner) = match parse(args) {
        Ok(parsed) => parsed,
        Err(e) => return crate::arg_err(&e),
    };
    let result = if is_inner {
        inner(&out)
    } else {
        job(&out).map(|job| {
            let none = "ci site: soothfast.toml has no [site] table";
            each::run_jobs(env, none, job.as_slice())
        })
    };
    result.unwrap_or_else(|e| {
        eprintln!("soothfast: {e}");
        2
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_site_builds_into_target_unless_told_otherwise() {
        assert_eq!(parse(&[]).unwrap(), ("target/soothfast-site".into(), false));
        assert_eq!(
            parse(&argv(&["--out", "public", "--inner"])).unwrap(),
            ("public".into(), true)
        );
        assert!(parse(&argv(&["--out"])).is_err());
        assert!(parse(&argv(&["--nope"])).is_err());
    }

    #[test]
    fn a_site_table_turns_the_build_on() {
        assert!(has_site("[site]\nname = \"x\"\n"));
        assert!(has_site(
            "[docs]\npackage = \"a\"\n\n[[site.nav]]\ntitle = \"t\"\n"
        ));
        assert!(!has_site("[docs]\npackage = \"a\"\n"));
        assert!(!has_site("[sites]\n"));
        assert!(!has_site(""));
    }

    #[test]
    fn a_hook_runs_under_the_same_record_as_the_build() {
        assert_eq!(job_args("out", false, false), command("out", false));
        assert_eq!(
            job_args("out", true, true),
            ["ci", "site", "--out", "out", "--inner"]
        );
    }

    #[test]
    fn the_head_baseline_is_used_when_there_is_one() {
        assert_eq!(command("out", false), ["docs", "build", "--out", "out"]);
        assert_eq!(
            command("out", true),
            ["docs", "build", "--out", "out", "--baseline", "head"]
        );
    }
}
