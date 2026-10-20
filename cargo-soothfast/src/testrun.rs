//! `cargo soothfast test`: run only the tests a change can affect.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::testbuild::{self, TestBin};
use crate::testexec::{self, Runner};
use crate::testkey::{self, Context, Passes};
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
    if let Some(why) = &selection.everything_because {
        println!("test: every package selected ({why})");
    }
    if selected.is_empty() {
        println!("{}", Tally::default().summary(0, pkgs.len()));
        return Ok(0);
    }
    let planned = plan(&args, &root, &cfg, &pkgs, &selected)?;
    let runner = Runner::new().map_err(|e| e.to_string())?;
    let passes = Passes::open();
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
        if runner
            .run(&p.bin, p.pkg, &args.run_args)
            .map_err(|e| format!("{label}: {e}"))?
        {
            if let (Some(key), Some(passes)) = (&p.key, &passes) {
                passes.record(key, &label);
            }
        } else {
            tally.failed.push(label);
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

/// Selected packages with a library target, whose doc tests cannot be cached.
fn doc_packages(args: &TestArgs, planned: &[Planned]) -> Vec<String> {
    if args.no_doc || testexec::narrows_targets(&args.cargo_args) {
        return Vec::new();
    }
    let names: BTreeSet<&str> = planned
        .iter()
        .filter(|p| matches!(p.bin.kind.as_str(), "lib" | "proc-macro"))
        .map(|p| p.bin.package.as_str())
        .collect();
    names.into_iter().map(String::from).collect()
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
struct Planned<'a> {
    bin: TestBin,
    pkg: &'a Package,
    key: Option<String>,
    cached: bool,
}

fn plan<'a>(
    args: &TestArgs,
    root: &Path,
    cfg: &test_config::TestConfig,
    pkgs: &'a [Package],
    selected: &[String],
) -> Result<Vec<Planned<'a>>, String> {
    let stdout = testbuild::build(selected, &args.cargo_args).map_err(|e| e.to_string())?;
    let built = testbuild::collect(&stdout, pkgs);
    let declared = testkey::declared_inputs(root, &cfg.inputs).map_err(|e| e.to_string())?;
    let rustc = testkey::rustc_version().map_err(|e| e.to_string())?;
    let cx = Context {
        inputs: &declared,
        run_args: &args.run_args,
        env: &cfg.env,
        rustc: &rustc,
    };
    let passes = Passes::open();
    let mut package_inputs: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut planned = Vec::new();
    for bin in built.tests {
        let Some(pkg) = pkgs.iter().find(|p| p.name == bin.package) else {
            continue;
        };
        if !package_inputs.contains_key(&bin.package) {
            let siblings: Vec<&TestBin> = built
                .siblings
                .iter()
                .filter(|s| s.package == bin.package)
                .collect();
            let digest = package_digest(root, pkg, &siblings)?;
            package_inputs.insert(bin.package.clone(), digest);
        }
        let key = package_inputs[&bin.package]
            .as_deref()
            .and_then(|files| testkey::key(&bin.package, &bin.target, &bin.exe, files, &cx));
        let cached = !args.no_cache
            && key
                .as_deref()
                .zip(passes.as_ref())
                .is_some_and(|(k, passes)| passes.has(k));
        planned.push(Planned {
            bin,
            pkg,
            key,
            cached,
        });
    }
    Ok(planned)
}

/// What a package's tests read besides their own code: its non-Rust files and
/// the executables its integration tests may spawn.
fn package_digest(
    root: &Path,
    pkg: &Package,
    siblings: &[&TestBin],
) -> Result<Option<String>, String> {
    let files = testkey::package_inputs(root, &pkg.dir).map_err(|e| e.to_string())?;
    Ok(testkey::siblings_digest(siblings).map(|s| format!("{files}\u{3}{s}")))
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
