//! `ci site`: builds the docs site when `soothfast.toml` configures one, so a
//! change that breaks the build fails before it is published.

use soothfast_site::toml::logical_lines;

use super::each::{self, Job};
use super::env::CiEnv;
use crate::invoke;

const DEFAULT_OUT: &str = "target/soothfast-site";

fn parse(args: &[String]) -> Result<String, String> {
    let mut out = DEFAULT_OUT.to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = it.next().cloned().ok_or("--out needs a value")?,
            other => return Err(format!("unknown ci site argument {other:?}")),
        }
    }
    Ok(out)
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

fn job(out: &str) -> Result<Option<Job>, String> {
    let root = invoke::workspace_root().map_err(|e| e.to_string())?;
    let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap_or_default();
    let head = invoke::baseline_path(&root, "head").is_file();
    Ok(has_site(&text).then(|| Job {
        name: "site".into(),
        args: command(out, head),
    }))
}

/// Entry point for `ci site`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let out = match parse(args) {
        Ok(out) => out,
        Err(e) => return crate::arg_err(&e),
    };
    match job(&out) {
        Ok(job) => each::run_jobs(
            env,
            "ci site: soothfast.toml has no [site] table",
            job.as_slice(),
        ),
        Err(e) => {
            eprintln!("soothfast: {e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_site_builds_into_target_unless_told_otherwise() {
        assert_eq!(parse(&[]).unwrap(), "target/soothfast-site");
        assert_eq!(parse(&argv(&["--out", "public"])).unwrap(), "public");
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
    fn the_head_baseline_is_used_when_there_is_one() {
        assert_eq!(command("out", false), ["docs", "build", "--out", "out"]);
        assert_eq!(
            command("out", true),
            ["docs", "build", "--out", "out", "--baseline", "head"]
        );
    }
}
