//! `cargo soothfast test`: run only the tests a change can affect.

use crate::testsel::{self, Package, Selection};
use crate::{invoke, test_config};

/// What the command line asked for.
#[derive(Default, Debug, PartialEq)]
pub struct TestArgs {
    /// `--base`: the ref changes are measured from.
    pub base: Option<String>,
    /// `--all`: select every package.
    pub all: bool,
    /// `--no-cache`: run cached passes again.
    pub no_cache: bool,
    /// `--no-doc`: skip doc tests.
    pub no_doc: bool,
    /// `-p`/`--package` values; narrow the selection.
    pub only: Vec<String>,
    /// `--exclude` values; drop from the selection.
    pub exclude: Vec<String>,
    /// Everything else before `--`, handed to `cargo test`.
    pub cargo_args: Vec<String>,
    /// Everything after `--`, handed to each test binary.
    pub run_args: Vec<String>,
}

/// Split the command line into soothfast's flags and cargo's.
pub fn parse_args(args: &[String]) -> Result<TestArgs, String> {
    let mut out = TestArgs::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match a.as_str() {
            "--" => {
                out.run_args = it.cloned().collect();
                break;
            }
            "--base" => out.base = Some(value("--base")?),
            "--all" => out.all = true,
            "--no-cache" => out.no_cache = true,
            "--no-doc" => out.no_doc = true,
            "-p" | "--package" => out.only.push(value(a)?),
            "--exclude" => out.exclude.push(value(a)?),
            "--workspace" => {}
            other => match other.strip_prefix("--package=") {
                Some(p) => out.only.push(p.to_string()),
                None => out.cargo_args.push(other.to_string()),
            },
        }
    }
    Ok(out)
}

/// Entry point for `cargo soothfast test`.
pub fn run(args: &[String]) -> i32 {
    match drive(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: {e}");
            2
        }
    }
}

fn drive(args: &[String]) -> Result<i32, String> {
    let args = parse_args(args)?;
    let root = invoke::workspace_root().map_err(|e| e.to_string())?;
    let cfg = test_config::load(&root)?;
    let pkgs = testsel::packages(&root).map_err(|e| e.to_string())?;
    let selection = choose(&args, &pkgs, &cfg)?;
    let selected = narrowed(&args, &pkgs, selection.names);
    match &selection.everything_because {
        Some(why) => println!(
            "test: {} of {} packages selected ({why})",
            selected.len(),
            pkgs.len()
        ),
        None => println!(
            "test: {} of {} packages selected",
            selected.len(),
            pkgs.len()
        ),
    }
    Ok(0)
}

fn choose(
    args: &TestArgs,
    pkgs: &[Package],
    cfg: &test_config::TestConfig,
) -> Result<Selection, String> {
    if args.all {
        return Ok(testsel::everything(pkgs, "--all"));
    }
    let base = match &args.base {
        Some(b) => b.clone(),
        None => testsel::default_base().map_err(|e| e.to_string())?,
    };
    let changed = testsel::changed_files(&base).map_err(|e| e.to_string())?;
    Ok(testsel::select(pkgs, &changed, cfg))
}

/// The selection after `-p` and `--exclude`.
fn narrowed(
    args: &TestArgs,
    pkgs: &[Package],
    names: std::collections::BTreeSet<String>,
) -> Vec<String> {
    pkgs.iter()
        .map(|p| p.name.as_str())
        .filter(|n| names.contains(*n))
        .filter(|n| args.only.is_empty() || args.only.iter().any(|o| o == n))
        .filter(|n| !args.exclude.iter().any(|e| e == n))
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(list: &[&str]) -> TestArgs {
        let argv: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        parse_args(&argv).unwrap()
    }

    #[test]
    fn soothfast_flags_are_taken_and_the_rest_goes_to_cargo() {
        let a = parse(&[
            "--base",
            "main",
            "--no-cache",
            "--all-features",
            "--release",
        ]);
        assert_eq!(a.base.as_deref(), Some("main"));
        assert!(a.no_cache);
        assert_eq!(a.cargo_args, ["--all-features", "--release"]);
    }

    #[test]
    fn args_after_the_separator_go_to_the_test_binaries() {
        let a = parse(&["--all", "--", "--include-ignored", "--base"]);
        assert!(a.all);
        assert_eq!(a.run_args, ["--include-ignored", "--base"]);
    }

    #[test]
    fn package_flags_narrow_instead_of_passing_through() {
        let a = parse(&["-p", "a", "--package=b", "--exclude", "c", "--workspace"]);
        assert_eq!(a.only, ["a", "b"]);
        assert_eq!(a.exclude, ["c"]);
        assert!(a.cargo_args.is_empty());
    }

    #[test]
    fn a_flag_missing_its_value_is_an_error() {
        assert!(parse_args(&["--base".to_string()]).is_err());
    }
}
