//! `cargo soothfast test`: run only the tests a change can affect.

use std::path::Path;

use crate::test_config::TestConfig;
use crate::testbuild::{self, TestBin};
use crate::testexec;
use crate::testkey::{self, Context, Passes};
use crate::testsel::{self, Package};
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
    let cfg = test_config::load(&root)?;
    let pkgs = testsel::packages(&root).map_err(|e| e.to_string())?;
    let selected = select_packages(&args, &root, &pkgs, &cfg)?;
    if selected.is_empty() {
        println!("{}", Tally::default().summary(0, pkgs.len()));
        return Ok(0);
    }
    let passes = Passes::open();
    let (keyer, planned) = plan(&args, &root, &cfg, &pkgs, &selected, passes.as_ref())?;
    let mut tally = Tally::default();
    for p in &planned {
        let label = p.bin.label();
        if p.cached {
            println!("test: skip {label} (cached pass)");
            tally.skipped += 1;
            continue;
        }
        println!("test: run {label}");
        tally.ran += 1;
        if !testexec::run_target(&p.bin, &args.cargo_args, &args.run_args)
            .map_err(|e| format!("{label}: {e}"))?
        {
            tally.failed.push(label);
        } else if let Some(passes) = &passes {
            let after = keyer.key(&p.bin)?;
            if !passes.record_if_unchanged(&label, p.key.as_deref(), after.as_deref()) {
                println!("test: {label} changed its inputs; pass not stored");
            }
        }
    }
    let doc = doc_packages(&args, &planned);
    if !doc.is_empty() {
        println!("test: doc tests for {}", doc.join(", "));
        tally.doc = doc.len();
        if !testexec::doc_tests(&doc, &args.cargo_args, &args.run_args)
            .map_err(|e| e.to_string())?
        {
            tally.failed.push("doc tests".into());
        }
    }
    println!("{}", tally.summary(selected.len(), pkgs.len()));
    Ok(i32::from(!tally.failed.is_empty()))
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
            testsel::everything(pkgs, "--all")
        } else {
            let base = match &args.base {
                Some(b) => b.clone(),
                None => testsel::default_base(root).map_err(|e| e.to_string())?,
            };
            let changed = testsel::changed_files(root, &base).map_err(|e| e.to_string())?;
            testsel::select(pkgs, &changed, cfg)
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
fn doc_packages(args: &TestArgs, planned: &[Planned]) -> Vec<String> {
    if args.no_doc || testexec::narrows_targets(&args.cargo_args) {
        return Vec::new();
    }
    let mut names: Vec<String> = planned
        .iter()
        .filter(|p| p.bin.has_doc_tests())
        .map(|p| p.bin.package.clone())
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

/// A built test executable and whether a stored pass already covers it.
struct Planned {
    bin: TestBin,
    key: Option<String>,
    cached: bool,
}

/// Computes pass cache keys against the files as they are right now.
struct Keyer<'a> {
    root: &'a Path,
    pkgs: &'a [Package],
    cfg: &'a TestConfig,
    run_args: &'a [String],
    siblings: Vec<TestBin>,
    rustc: String,
    env: Vec<(String, String)>,
}

impl Keyer<'_> {
    fn key(&self, bin: &TestBin) -> Result<Option<String>, String> {
        let Some(pkg) = self.pkgs.iter().find(|p| p.name == bin.package) else {
            return Ok(None);
        };
        let siblings: Vec<&TestBin> = self
            .siblings
            .iter()
            .filter(|s| s.package == bin.package)
            .collect();
        let Some(spawned) = testkey::siblings_digest(&siblings) else {
            return Ok(None);
        };
        let err = |e: std::io::Error| e.to_string();
        let files = testkey::package_inputs(self.root, &pkg.dir).map_err(err)?;
        let declared = testkey::declared_inputs(self.root, &self.cfg.inputs).map_err(err)?;
        let workspace = testkey::workspace_inputs(self.root, self.pkgs, self.cfg).map_err(err)?;
        let cx = Context {
            inputs: &declared,
            workspace: &workspace,
            run_args: self.run_args,
            env: &self.env,
            rustc: &self.rustc,
        };
        let package_inputs = format!("{files}\u{3}{spawned}");
        Ok(testkey::key(
            &bin.package,
            &bin.target,
            &bin.exe,
            &package_inputs,
            &cx,
        ))
    }
}

fn plan<'a>(
    args: &'a TestArgs,
    root: &'a Path,
    cfg: &'a TestConfig,
    pkgs: &'a [Package],
    selected: &[String],
    passes: Option<&Passes>,
) -> Result<(Keyer<'a>, Vec<Planned>), String> {
    let messages = testbuild::build(selected, &args.cargo_args).map_err(|e| e.to_string())?;
    let built = testbuild::collect(&messages, pkgs);
    let keyer = Keyer {
        root,
        pkgs,
        cfg,
        run_args: &args.run_args,
        siblings: built.siblings,
        rustc: testkey::rustc_version().map_err(|e| e.to_string())?,
        env: testkey::env_values(&cfg.env),
    };
    let mut planned = Vec::new();
    for bin in built.tests {
        let key = keyer.key(&bin)?;
        let cached = !args.no_cache
            && key
                .as_deref()
                .zip(passes)
                .is_some_and(|(k, passes)| passes.has(k));
        planned.push(Planned { bin, key, cached });
    }
    Ok((keyer, planned))
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
