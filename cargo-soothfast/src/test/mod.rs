//! `cargo soothfast test`: run only the tests a change can affect.

mod cargo;
mod config;
#[cfg(test)]
mod fixture;
mod key;
mod select;
pub(crate) mod shim;

use std::path::Path;

use crate::invoke;
use cargo::TestBin;
use config::TestConfig;
use key::{Keyer, Passes};
use select::Package;

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
    /// `-p`/`--package` values; select exactly these packages.
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
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--" => {
                out.run_args = it.cloned().collect();
                break;
            }
            "--base" => out.base = Some(value()?),
            "--all" => out.all = true,
            "--no-cache" => out.no_cache = true,
            "--no-doc" => out.no_doc = true,
            "--doc" => return Err("doc tests always run; use --no-doc to skip them".into()),
            "-p" | "--package" => out.only.push(value()?),
            "--exclude" => out.exclude.push(value()?),
            "--workspace" => {}
            _ => match a.strip_prefix("-p") {
                Some(name) if !name.is_empty() => out.only.push(name.to_string()),
                _ => out.cargo_args.push(a.clone()),
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
    let cfg = config::load(&root)?;
    let pkgs = select::packages(&root).map_err(|e| e.to_string())?;
    let total = pkgs.len();
    let selected = select_packages(&args, &root, &pkgs, &cfg)?;
    let mut tally = Tally::default();
    if selected.is_empty() {
        println!("{}", tally.summary(0, total));
        return Ok(0);
    }
    let messages = cargo::build(&selected, &args.cargo_args).map_err(|e| e.to_string())?;
    let built = cargo::collect(&messages, &pkgs);
    let mut keyer = Keyer::new(root, pkgs, cfg, args.run_args.clone(), built.siblings)?;
    run_binaries(&args, &built.tests, &mut keyer, &mut tally)?;
    let doc = doc_packages(&args, &built.tests);
    if !doc.is_empty() {
        println!("test: doc tests for {}", doc.join(", "));
        tally.doc = doc.len();
        if !cargo::doc_tests(&doc, &args.cargo_args, &args.run_args).map_err(|e| e.to_string())? {
            tally.failed.push("doc tests".into());
        }
    }
    println!("{}", tally.summary(selected.len(), total));
    Ok(i32::from(!tally.failed.is_empty()))
}

/// Run each test binary a stored pass does not cover. A pass is stored only
/// if the binary's key is the same after it ran: a test that rewrites its own
/// inputs would otherwise record a pass for inputs that no longer exist.
fn run_binaries(
    args: &TestArgs,
    tests: &[TestBin],
    keyer: &mut Keyer,
    tally: &mut Tally,
) -> Result<(), String> {
    let passes = Passes::open();
    for bin in tests {
        let label = bin.label();
        let key = keyer.key(bin)?;
        let cached = !args.no_cache
            && key
                .as_deref()
                .zip(passes.as_ref())
                .is_some_and(|(k, passes)| passes.has(k));
        if cached {
            println!("test: skip {label} (cached pass)");
            tally.skipped += 1;
            continue;
        }
        println!("test: run {label}");
        tally.ran += 1;
        if !cargo::run_target(bin, &args.cargo_args, &args.run_args)
            .map_err(|e| format!("{label}: {e}"))?
        {
            tally.failed.push(label);
            continue;
        }
        let Some(passes) = &passes else {
            continue;
        };
        if keyer.rekey(bin)? != key {
            println!("test: {label} changed its inputs; pass not stored");
        } else if let Some(key) = &key {
            passes.record(&label, key);
        }
    }
    Ok(())
}

/// The packages to test: exactly those named with `-p`, else what the
/// change selects (or `--all`), minus `--exclude`.
fn select_packages(
    args: &TestArgs,
    root: &Path,
    pkgs: &[Package],
    cfg: &TestConfig,
) -> Result<Vec<String>, String> {
    let names: Vec<String> = if args.only.is_empty() {
        let selection = if args.all {
            select::everything(pkgs, "--all")
        } else {
            changed(args, root, pkgs, cfg).map_err(|e| e.to_string())?
        };
        if let Some(why) = &selection.everything_because {
            println!("test: every package selected ({why})");
        }
        selection.names.into_iter().collect()
    } else {
        named(args, pkgs)?
    };
    Ok(names
        .into_iter()
        .filter(|n| !args.exclude.contains(n))
        .collect())
}

/// What the files changed since the base select, or every package when the
/// base shares no history with HEAD.
fn changed(
    args: &TestArgs,
    root: &Path,
    pkgs: &[Package],
    cfg: &TestConfig,
) -> std::io::Result<select::Selection> {
    let base = match &args.base {
        Some(b) => b.clone(),
        None => select::default_base(root)?,
    };
    if !select::has_merge_base(root, &base)? {
        return Ok(select::everything(
            pkgs,
            &format!("no merge-base with {base}"),
        ));
    }
    let changed = select::changed_files(root, &base)?;
    Ok(select::select(pkgs, &changed, cfg))
}

fn named(args: &TestArgs, pkgs: &[Package]) -> Result<Vec<String>, String> {
    for name in &args.only {
        if !pkgs.iter().any(|p| &p.name == name) {
            let members: Vec<&str> = pkgs.iter().map(|p| p.name.as_str()).collect();
            return Err(format!(
                "no workspace package {name:?}; members: {}",
                members.join(", ")
            ));
        }
    }
    Ok(pkgs
        .iter()
        .map(|p| p.name.clone())
        .filter(|n| args.only.contains(n))
        .collect())
}

/// Selected packages with a library target, whose doc tests cannot be cached.
fn doc_packages(args: &TestArgs, tests: &[TestBin]) -> Vec<String> {
    if args.no_doc || cargo::narrows_targets(&args.cargo_args) {
        return Vec::new();
    }
    let mut names: Vec<String> = tests
        .iter()
        .filter(|b| b.has_doc_tests())
        .map(|b| b.package.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

#[derive(Default)]
struct Tally {
    ran: usize,
    skipped: usize,
    doc: usize,
    failed: Vec<String>,
}

impl Tally {
    fn summary(&self, selected: usize, total: usize) -> String {
        let mut line = format!(
            "test: {selected} of {total} packages selected, {} binaries run, {} skipped (cached), doc tests in {} packages",
            self.ran, self.skipped, self.doc
        );
        if !self.failed.is_empty() {
            line.push_str(&format!("\ntest: FAILED: {}", self.failed.join(", ")));
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(list: &[&str]) -> TestArgs {
        let argv: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        parse_args(&argv).unwrap()
    }

    fn pkg(name: &str) -> Package {
        Package {
            name: name.into(),
            dir: name.into(),
            manifest_dir: name.into(),
            deps: Vec::new(),
        }
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
    fn package_flags_in_every_spelling_are_taken() {
        let a = parse(&[
            "-p",
            "a",
            "--package=b",
            "-pc",
            "--package",
            "d",
            "--exclude=e",
            "--workspace",
        ]);
        assert_eq!(a.only, ["a", "b", "c", "d"]);
        assert_eq!(a.exclude, ["e"]);
        assert!(a.cargo_args.is_empty());
    }

    #[test]
    fn equals_joined_values_reach_their_owners() {
        let a = parse(&["--base=origin/main", "--test=golden", "--profile=release"]);
        assert_eq!(a.base.as_deref(), Some("origin/main"));
        assert_eq!(a.cargo_args, ["--test=golden", "--profile=release"]);
    }

    #[test]
    fn doc_is_refused_with_a_hint() {
        let err = parse_args(&["--doc".to_string()]).unwrap_err();
        assert!(err.contains("--no-doc"));
    }

    #[test]
    fn p_selects_exactly_the_named_packages() {
        let pkgs = [pkg("a"), pkg("b"), pkg("c")];
        let args = TestArgs {
            only: vec!["c".into(), "a".into()],
            ..TestArgs::default()
        };
        assert_eq!(named(&args, &pkgs).unwrap(), ["a", "c"]);
    }

    #[test]
    fn an_unknown_package_is_an_error_listing_the_members() {
        let args = TestArgs {
            only: vec!["soothfast-sit".into()],
            ..TestArgs::default()
        };
        let err = named(&args, &[pkg("a"), pkg("b")]).unwrap_err();
        assert!(err.contains("soothfast-sit") && err.contains("a, b"));
    }

    #[test]
    fn the_summary_counts_work_and_names_failures() {
        let tally = Tally {
            ran: 3,
            skipped: 5,
            doc: 2,
            failed: vec!["p t (lib)".into()],
        };
        assert_eq!(
            tally.summary(4, 10),
            "test: 4 of 10 packages selected, 3 binaries run, 5 skipped (cached), doc tests in 2 packages\ntest: FAILED: p t (lib)"
        );
    }

    #[test]
    fn a_flag_missing_its_value_is_an_error() {
        assert!(parse_args(&["--base".to_string()]).is_err());
    }
}
