//! `cargo soothfast test`: run only the tests a change can affect.

mod cargo;
pub(crate) mod config;
#[cfg(test)]
pub(crate) mod fixture;
mod key;
pub(crate) mod project;
mod runner;
pub(crate) mod rustdoc;
mod select;
pub(crate) mod shim;

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::{buildstamp, invoke, runcache};
use cargo::TestBin;
use config::TestConfig;
use key::{Keyer, Passes};
use select::Package;
use shim::{DocSkip, Entry, Plan};

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
    /// `--no-run`: build the test binaries and run nothing.
    pub no_run: bool,
    /// `--result`: write the outcome here as JSON.
    pub result: Option<PathBuf>,
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
            "--no-run" => out.no_run = true,
            "--doc" => return Err("doc tests always run; use --no-doc to skip them".into()),
            "-p" | "--package" => out.only.push(value()?),
            "--exclude" => out.exclude.push(value()?),
            "--result" => out.result = Some(PathBuf::from(value()?)),
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
    if args
        .iter()
        .any(|a| a == "--project" || a.starts_with("--project="))
    {
        return project::run(args);
    }
    match drive(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: {e}");
            2
        }
    }
}

fn drive(args: &[String]) -> Result<i32, String> {
    let started = Instant::now();
    let args = parse_args(args)?;
    let root = invoke::workspace_root().map_err(|e| e.to_string())?;
    let cfg = config::load(&root)?;
    let retries = cfg.retries;
    let pkgs = select::packages(&root).map_err(|e| e.to_string())?;
    cfg.check_packages(pkgs.iter().map(|p| p.name.as_str()))?;
    let total = pkgs.len();
    let selected = select_packages(&args, &root, &pkgs, &cfg)?;
    let mut tally = Tally::default();
    if selected.is_empty() {
        println!("{}", tally.summary(0, total));
        write_result(&args, &tally, 0, total, started)?;
        return Ok(0);
    }
    let run_dir = RunDir::create()?;
    let messages = cargo::build(&selected, &args.cargo_args).map_err(|e| e.to_string())?;
    let built = cargo::collect(&messages, &pkgs);
    if args.no_run {
        println!(
            "test: {} of {total} packages selected, {} binaries built, not run (--no-run)",
            selected.len(),
            built.tests.len()
        );
        return Ok(0);
    }
    let triples = runner::triples(
        &host_triple()?,
        &args.cargo_args,
        std::env::var("CARGO_BUILD_TARGET").ok(),
    );
    let cargo_home = runner::cargo_home(|k| std::env::var(k).ok());
    let mode = runner::detect(
        &triples,
        &args.cargo_args,
        |k| std::env::var(k).ok(),
        &runner::config_files(&root, cargo_home),
    );
    let mut keyer = Keyer::new(
        root.clone(),
        pkgs,
        cfg,
        args.run_args.clone(),
        built.siblings,
    )?;
    let doc = doc_packages(&args, &built.tests);
    match mode {
        runner::Runner::Shim(chained) => {
            let run = Together {
                run_dir: &run_dir,
                selected: &selected,
                triples: &triples,
                chained,
                root: &root,
                doc: &doc,
            };
            run_together(&args, &built.tests, &run, &mut keyer, &mut tally)?;
        }
        runner::Runner::Fallback(why) => {
            println!("test: running each binary through its own cargo test: {why}");
            run_binaries(&args, &built.tests, &mut keyer, &mut tally)?;
            run_doc_tests(&args, &doc, &mut tally)?;
        }
    }
    rerun_failed(&mut tally, retries, |label| {
        let Some(bin) = built.tests.iter().find(|b| b.label() == label) else {
            return Ok(false);
        };
        println!("test: rerunning {label}");
        cargo::run_target(bin, &args.cargo_args, &args.run_args)
            .map_err(|e| format!("{label}: {e}"))
    })?;
    println!("{}", tally.summary(selected.len(), total));
    write_result(&args, &tally, selected.len(), total, started)?;
    Ok(i32::from(!tally.failed.is_empty()))
}

/// Rerun each failed binary up to `retries` times. A binary that passes a
/// rerun moves from failed to flaky and has no pass stored.
fn rerun_failed(
    tally: &mut Tally,
    retries: u32,
    mut rerun: impl FnMut(&str) -> Result<bool, String>,
) -> Result<(), String> {
    for label in tally.failed.clone() {
        for _ in 0..retries {
            if rerun(&label)? {
                tally.failed.retain(|l| *l != label);
                tally.flaky.push(label);
                break;
            }
        }
    }
    Ok(())
}

fn write_result(
    args: &TestArgs,
    tally: &Tally,
    selected: usize,
    total: usize,
    started: Instant,
) -> Result<(), String> {
    let Some(path) = &args.result else {
        return Ok(());
    };
    let output: Vec<String> = tally
        .summary(selected, total)
        .lines()
        .map(String::from)
        .collect();
    let wall_s = started.elapsed().as_secs_f64();
    let verdict = tally.verdict(selected);
    project::write_result(path, "test-rust", ".", verdict, &output, wall_s)
}

fn host_triple() -> Result<String, String> {
    let vv = buildstamp::rustc_vv(None).map_err(|e| e.to_string())?;
    vv.lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(String::from)
        .ok_or_else(|| "rustc -vV names no host".into())
}

fn run_doc_tests(args: &TestArgs, doc: &[String], tally: &mut Tally) -> Result<(), String> {
    if doc.is_empty() {
        return Ok(());
    }
    println!("test: doc tests for {}", doc.join(", "));
    tally.doc = doc.len();
    if !cargo::doc_tests(doc, &args.cargo_args, &args.run_args).map_err(|e| e.to_string())? {
        tally.failed.push("doc tests".into());
    }
    Ok(())
}

/// The run step's scratch directory, holding a copy of this executable taken
/// before the build: the build can relink the binary this process started
/// from, as when soothfast tests its own workspace.
struct RunDir {
    dir: PathBuf,
    exe: PathBuf,
}

impl RunDir {
    fn create() -> Result<RunDir, String> {
        let dir = runcache::state_dir(&format!("test-run/{}", std::process::id()))
            .ok_or("cannot create .soothfast/test-run")?;
        let exe = dir.join(format!("soothfast-shim{}", std::env::consts::EXE_SUFFIX));
        let mut current = std::env::current_exe().map_err(|e| e.to_string())?;
        if cfg!(target_os = "linux") && !current.is_file() {
            current = PathBuf::from("/proc/self/exe");
        }
        rustdoc::link_or_copy(&current, &exe).map_err(|e| e.to_string())?;
        Ok(RunDir { dir, exe })
    }
}

impl Drop for RunDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// What the single run step needs beyond the parsed arguments.
struct Together<'a> {
    run_dir: &'a RunDir,
    selected: &'a [String],
    triples: &'a [String],
    chained: Vec<String>,
    root: &'a Path,
    doc: &'a [String],
}

/// Run every selected binary in one `cargo test` that matches the build, the
/// shim skipping those with a stored pass. Doc tests run in the same
/// invocation unless `--no-doc` has the rustdoc wrapper skip them.
fn run_together(
    args: &TestArgs,
    tests: &[TestBin],
    run: &Together,
    keyer: &mut Keyer,
    tally: &mut Tally,
) -> Result<(), String> {
    let passes = Passes::open();
    let mut planned = Vec::new();
    for bin in tests {
        let key = keyer.key(bin)?;
        let cached = !args.no_cache
            && key
                .as_deref()
                .zip(passes.as_ref())
                .is_some_and(|(k, passes)| passes.has(k));
        if cached {
            tally.skipped += 1;
        } else {
            tally.ran += 1;
        }
        planned.push((bin, key, cached));
    }
    let dir = run.run_dir.dir.clone();
    let skip_doc = args.no_doc && !cargo::narrows_targets(&args.cargo_args);
    let plan = Plan {
        results: dir.clone(),
        runner: run.chained.clone(),
        entries: planned
            .iter()
            .map(|(bin, _, cached)| {
                let entry = Entry {
                    label: bin.label(),
                    cached: *cached,
                };
                (bin.exe.clone(), entry)
            })
            .collect(),
        doc_skip: skip_doc.then(|| DocSkip {
            original: std::env::var("RUSTDOC").ok(),
            crates: tests
                .iter()
                .filter(|b| b.has_doc_tests())
                .map(|b| b.target.replace('-', "_"))
                .collect(),
            root: run.root.to_path_buf(),
        }),
    };
    let plan_path = dir.join(rustdoc::PLAN_FILE);
    plan.write(&plan_path).map_err(|e| e.to_string())?;
    let wrapper = match skip_doc {
        true => Some(rustdoc::install(&dir, &run.run_dir.exe).map_err(|e| e.to_string())?),
        false => None,
    };
    if !skip_doc && !run.doc.is_empty() {
        println!("test: doc tests for {}", run.doc.join(", "));
        tally.doc = run.doc.len();
    }
    let outcome = cargo::run_step(&cargo::RunStep {
        names: run.selected,
        cargo_args: &args.cargo_args,
        run_args: &args.run_args,
        triples: run.triples,
        plan: &plan_path,
        shim: &run.run_dir.exe,
        rustdoc: wrapper.as_deref(),
    })
    .map_err(|e| e.to_string())?;
    let failed_before = tally.failed.len();
    for (bin, key, _) in planned.iter().filter(|(_, _, cached)| !cached) {
        let label = bin.label();
        match plan.status(&bin.exe) {
            Some(0) => store_pass(keyer, passes.as_ref(), bin, key.as_deref(), &label)?,
            Some(_) => tally.failed.push(label),
            None => {
                println!("test: {label} did not finish");
                tally.failed.push(label);
            }
        }
    }
    if !outcome.success {
        attribute_cargo_failure(&outcome, tally.failed.len() > failed_before, tally);
    }
    Ok(())
}

/// A pass is stored only if the binary's key is the same after it ran: a
/// test that rewrites its own inputs would otherwise record a pass for inputs
/// that no longer exist.
fn store_pass(
    keyer: &mut Keyer,
    passes: Option<&Passes>,
    bin: &TestBin,
    key: Option<&str>,
    label: &str,
) -> Result<(), String> {
    let Some(passes) = passes else {
        return Ok(());
    };
    if keyer.rekey(bin)?.as_deref() != key {
        println!("test: {label} changed its inputs; pass not stored");
    } else if let Some(key) = key {
        passes.record(label, key);
    }
    Ok(())
}

/// Name what failed when cargo exited nonzero. A binary's failure is already
/// counted; anything else is a doc test only when cargo's output says so.
fn attribute_cargo_failure(outcome: &cargo::RunOutcome, binary_failed: bool, tally: &mut Tally) {
    if outcome.doc_failed {
        tally.failed.push("doc tests".into());
    } else if !binary_failed {
        let code = outcome.code.map_or("by a signal".into(), |c| c.to_string());
        println!("test: cargo test exited {code} after every test binary passed:");
        for line in &outcome.tail {
            println!("  {line}");
        }
        tally.failed.push(format!("cargo test (exit {code})"));
    }
}

/// Run each test binary a stored pass does not cover through its own
/// `cargo test`.
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
        store_pass(keyer, passes.as_ref(), bin, key.as_deref(), &label)?;
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
    /// Binaries that failed, then passed on a rerun.
    flaky: Vec<String>,
}

impl Tally {
    fn summary(&self, selected: usize, total: usize) -> String {
        let mut line = format!(
            "test: {selected} of {total} packages selected, {} binaries run, {} skipped (cached), doc tests in {} packages",
            self.ran, self.skipped, self.doc
        );
        if !self.flaky.is_empty() {
            line.push_str(&format!("\ntest: FLAKY: {}", self.flaky.join(", ")));
        }
        if !self.failed.is_empty() {
            line.push_str(&format!("\ntest: FAILED: {}", self.failed.join(", ")));
        }
        line
    }

    fn verdict(&self, selected: usize) -> project::Verdict {
        use project::Verdict;
        if !self.failed.is_empty() {
            Verdict::Failed
        } else if selected == 0 {
            Verdict::Skipped
        } else if !self.flaky.is_empty() {
            Verdict::Flaky
        } else if self.ran == 0 {
            Verdict::Reused
        } else {
            Verdict::Pass
        }
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
    fn a_failed_binary_that_passes_a_rerun_is_flaky() {
        let mut tally = Tally {
            failed: vec!["a".into(), "b".into()],
            ..Tally::default()
        };
        let mut calls = Vec::new();
        rerun_failed(&mut tally, 2, |label| {
            calls.push(label.to_string());
            Ok(label == "a" && calls.len() == 2)
        })
        .unwrap();
        assert_eq!(calls, ["a", "a", "b", "b"]);
        assert_eq!(
            (tally.failed.as_slice(), tally.flaky.as_slice()),
            (&["b".to_string()][..], &["a".to_string()][..])
        );
        assert!(
            tally
                .summary(2, 3)
                .contains("test: FLAKY: a\ntest: FAILED: b")
        );
        assert_eq!(tally.verdict(2), project::Verdict::Failed);
        tally.failed.clear();
        assert_eq!(tally.verdict(2), project::Verdict::Flaky);
        assert_eq!(tally.verdict(0), project::Verdict::Skipped);
    }

    #[test]
    fn no_retries_reruns_nothing() {
        let mut tally = Tally {
            failed: vec!["a".into()],
            ..Tally::default()
        };
        rerun_failed(&mut tally, 0, |_| panic!("reran")).unwrap();
        assert_eq!(tally.failed, ["a"]);
    }

    #[test]
    fn a_run_of_only_cached_binaries_is_reused() {
        let tally = Tally {
            skipped: 3,
            ..Tally::default()
        };
        assert_eq!(tally.verdict(1), project::Verdict::Reused);
        let ran = Tally {
            ran: 1,
            ..Tally::default()
        };
        assert_eq!(ran.verdict(1), project::Verdict::Pass);
    }

    #[test]
    fn the_result_flag_is_ours_and_not_cargos() {
        let args = parse(&["--result", "r.json", "--release"]);
        assert_eq!(args.result, Some(PathBuf::from("r.json")));
        assert_eq!(args.cargo_args, ["--release"]);
    }

    #[test]
    fn no_run_is_a_soothfast_flag_and_not_passed_to_cargo() {
        let a = parse(&["--no-run", "--release"]);
        assert!(a.no_run);
        assert_eq!(a.cargo_args, vec!["--release"]);
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
            ..Tally::default()
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

    fn outcome(doc_failed: bool) -> cargo::RunOutcome {
        cargo::RunOutcome {
            success: false,
            code: Some(101),
            doc_failed,
            tail: vec!["error: could not compile `p`".into()],
        }
    }

    #[test]
    fn a_failure_cargo_names_as_a_doc_test_is_one() {
        let mut tally = Tally::default();
        attribute_cargo_failure(&outcome(true), false, &mut tally);
        assert_eq!(tally.failed, ["doc tests"]);
    }

    #[test]
    fn a_nonzero_exit_after_every_binary_passed_is_not_called_a_doc_test() {
        let mut tally = Tally::default();
        attribute_cargo_failure(&outcome(false), false, &mut tally);
        assert_eq!(tally.failed, ["cargo test (exit 101)"]);
    }

    #[test]
    fn a_binary_failure_explains_the_exit_by_itself() {
        let mut tally = Tally::default();
        attribute_cargo_failure(&outcome(false), true, &mut tally);
        assert!(tally.failed.is_empty());
    }
}
