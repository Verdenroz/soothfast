//! `ci gate-import`: the perf gate of a project outside the cargo workspace.
//! It benches the base and the head with the project's own harness and gates
//! the imported results.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::env::{CiEnv, Event};
use super::record::run_recorded;
use crate::init::ecosystem::{Ecosystem, Project, normalize_dir};

/// The exit code of a job that errored, as opposed to one that regressed.
const ERRORED: i32 = 3;

/// One `[[project]]` with a benchmark command, ready to gate.
#[derive(Debug, PartialEq)]
struct Job {
    name: String,
    ecosystem: Ecosystem,
    dir: String,
    cmd: String,
    out: String,
    from: String,
    walltime: Option<String>,
    prefix: Option<String>,
}

/// The files a job leaves under the runner's temporary directory, where the
/// workflow's later steps look for them.
#[derive(Debug, PartialEq)]
struct Scratch(PathBuf);

impl Scratch {
    fn baseline(&self, name: &str) -> PathBuf {
        self.0
            .join("soothfast-baseline")
            .join(format!("import-{name}.json"))
    }

    fn base_results(&self, name: &str) -> PathBuf {
        self.0.join(format!("soothfast-base-results-{name}"))
    }

    fn save_failed(&self, name: &str) -> PathBuf {
        self.0.join(format!("soothfast-save-failed-{name}"))
    }
}

/// What the bench commands of the two sides decide, before the gate runs.
#[derive(Debug, PartialEq)]
enum Sides {
    HeadFailed,
    NothingToCompare,
    Compare,
}

/// A side passes when its bench exited 0 and left its results file.
fn sides(head_ok: bool, base_ok: bool) -> Sides {
    match (head_ok, base_ok) {
        (false, _) => Sides::HeadFailed,
        (true, false) => Sides::NothingToCompare,
        (true, true) => Sides::Compare,
    }
}

/// What the gate's exit code means for the job.
#[derive(Debug, PartialEq)]
enum Gated {
    Passed,
    /// Regressions are measured numbers, so HEAD's items are still saved.
    Regressed,
    Errored(i32),
}

fn gated(code: i32) -> Gated {
    match code {
        0 => Gated::Passed,
        1 => Gated::Regressed,
        other => Gated::Errored(other),
    }
}

impl Job {
    /// The command the project benches with, run from its directory.
    fn bench_script(&self) -> String {
        let cmd = self.cmd.replace("{out}", &self.out);
        match self.ecosystem {
            Ecosystem::Node => format!("npm ci\n{cmd}"),
            _ => cmd,
        }
    }

    fn save_args(&self, to: &Path) -> Vec<String> {
        let mut args = vec!["import".into(), "--from".into(), self.from.clone()];
        args.push(self.out.clone());
        args.extend(self.prefix_args());
        args.extend(["--out".into(), to.display().to_string()]);
        args
    }

    fn gate_args(&self, reference: &Path) -> Vec<String> {
        let mut args = vec!["gate".into(), "--import".into(), self.from.clone()];
        args.push(self.out.clone());
        args.extend(["--reference-import".into(), self.from.clone()]);
        args.push(reference.display().to_string());
        args.extend(self.prefix_args());
        if let Some(walltime) = &self.walltime {
            args.extend(["--walltime".into(), walltime.clone()]);
        }
        args
    }

    /// The import of a gate that saved nothing: the items it covers, named
    /// from the results it has, else the base's, else only that it failed.
    fn failed_args(
        &self,
        to: &Path,
        base_results: Option<&Path>,
        save_failed: bool,
    ) -> Vec<String> {
        let mut args = self.save_args(to);
        args.push("--failed".into());
        if let Some(base) = base_results {
            args.extend(["--base-results".into(), base.display().to_string()]);
        }
        if save_failed {
            args.push("--save-failed".into());
        }
        args
    }

    fn prefix_args(&self) -> Vec<String> {
        match &self.prefix {
            Some(prefix) => vec!["--prefix".into(), prefix.clone()],
            None => Vec::new(),
        }
    }
}

/// The jobs for `dir`, or for every project that benches. A project's name
/// is the one the workflow's matrix gives it unless `name` overrides it.
fn jobs(projects: &[Project], dir: Option<&str>, name: Option<&str>) -> Result<Vec<Job>, String> {
    let names = crate::plan::names(projects);
    let mut jobs = Vec::new();
    for (project, default) in projects.iter().zip(names) {
        let (Some(bench), true) = (&project.bench, dir.is_none_or(|d| d == project.dir)) else {
            continue;
        };
        jobs.push(Job {
            name: name.map(String::from).unwrap_or(default),
            ecosystem: project.ecosystem,
            dir: project.dir.clone(),
            cmd: bench.cmd.clone(),
            out: bench.out.clone(),
            from: bench.from.clone(),
            walltime: project.walltime.clone(),
            prefix: project.prefix.clone(),
        });
    }
    match (dir, jobs.is_empty()) {
        (Some(dir), true) => Err(format!(
            "{dir} is not a [[project]] with a benchmark command"
        )),
        _ => Ok(jobs),
    }
}

#[derive(Debug, Default, PartialEq)]
struct Args {
    project: Option<String>,
    name: Option<String>,
    accepted: Option<String>,
    inner: bool,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match a.as_str() {
            "--project" => parsed.project = Some(normalize_dir(&value(a)?)?),
            "--name" => parsed.name = Some(value(a)?),
            "--accepted" => parsed.accepted = Some(value(a)?),
            "--inner" => parsed.inner = true,
            other => return Err(format!("unknown ci gate-import argument {other:?}")),
        }
    }
    match (&parsed.project, &parsed.name, parsed.inner) {
        (None, Some(_), _) => Err("--name needs --project".into()),
        (None, _, true) => Err("--inner needs --project".into()),
        _ => Ok(parsed),
    }
}

/// Entry point for `ci gate-import`. Every project is gated on its own and
/// the exit code is the worst of them. `--accepted LABEL` waives a
/// regression, never an error.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let args = match parse(args) {
        Ok(args) => args,
        Err(e) => return crate::arg_err(&e),
    };
    let root = crate::invoke::project_root();
    let projects = match crate::init::manifest(&root) {
        Ok(projects) => projects,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    let jobs = match jobs(&projects, args.project.as_deref(), args.name.as_deref()) {
        Ok(jobs) => jobs,
        Err(e) => return crate::arg_err(&e),
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    if args.inner {
        return jobs.first().map_or(2, |job| inner(env, &root, &exe, job));
    }
    gate_all(&jobs, args.accepted.as_deref(), |job| {
        launch(env, &exe, job)
    })
}

fn gate_all(jobs: &[Job], accepted: Option<&str>, mut launch: impl FnMut(&Job) -> i32) -> i32 {
    jobs.iter()
        .map(|job| settle(launch(job), accepted))
        .max()
        .unwrap_or(0)
}

/// A regression the label accepts passes; an error never does.
fn settle(code: i32, accepted: Option<&str>) -> i32 {
    match (code, accepted) {
        (1, Some(label)) => {
            eprintln!("soothfast: regressions accepted by the `{label}` label");
            0
        }
        _ => code,
    }
}

fn event_name(event: Event) -> &'static str {
    match event {
        Event::PullRequest => "pull_request",
        Event::Push => "push",
        Event::Other => "other",
    }
}

/// How a job re-runs itself without recording, so [`run_recorded`] can
/// capture its output and exit code as the record.
fn inner_args(env: &CiEnv, job: &Job) -> Vec<String> {
    let mut args = vec!["ci".into(), "--event".into(), event_name(env.event).into()];
    args.extend(env.base.iter().flat_map(|b| ["--base".into(), b.clone()]));
    args.extend(["gate-import".into(), "--project".into(), job.dir.clone()]);
    args.extend(["--name".into(), job.name.clone(), "--inner".into()]);
    args
}

fn launch(env: &CiEnv, exe: &Path, job: &Job) -> i32 {
    let mut cmd = Command::new(exe);
    cmd.args(inner_args(env, job));
    run_recorded(env, &job.name, "import", &mut cmd)
}

/// Only a pull request has a base to compare with; a push records the head's
/// numbers alone.
fn inner(env: &CiEnv, root: &Path, exe: &Path, job: &Job) -> i32 {
    let scratch =
        Scratch(std::env::var_os("RUNNER_TEMP").map_or_else(std::env::temp_dir, Into::into));
    let var = |key: &str| std::env::var(key).unwrap_or_else(|_| std::process::id().to_string());
    let tree = scratch.0.join(format!(
        "soothfast-base-{}-{}-{}",
        var("GITHUB_RUN_ID"),
        var("GITHUB_RUN_ATTEMPT"),
        job.name
    ));
    let ctx = Ctx {
        root,
        exe,
        scratch: &scratch,
        base: compare_base(env),
        tree,
    };
    gate(&mut System, &ctx, job)
}

fn compare_base(env: &CiEnv) -> Option<&str> {
    env.base
        .as_deref()
        .filter(|_| env.event == Event::PullRequest)
}

/// What the gate needs from the machine: running commands, and the few file
/// operations around them.
trait Host {
    fn run(&mut self, dir: &Path, program: &Path, args: &[String]) -> i32;
    fn output(&mut self, dir: &Path, program: &Path, args: &[String]) -> Option<String>;
    fn exists(&self, path: &Path) -> bool;
    fn copy(&mut self, from: &Path, to: &Path) -> bool;
    fn mkdir(&mut self, path: &Path) -> bool;
    fn touch(&mut self, path: &Path);
    fn remove(&mut self, path: &Path);
}

struct System;

impl Host for System {
    fn run(&mut self, dir: &Path, program: &Path, args: &[String]) -> i32 {
        match Command::new(program).args(args).current_dir(dir).status() {
            Ok(status) => status.code().unwrap_or(ERRORED),
            Err(e) => {
                eprintln!("soothfast: {}: {e}", program.display());
                ERRORED
            }
        }
    }

    fn output(&mut self, dir: &Path, program: &Path, args: &[String]) -> Option<String> {
        let out = Command::new(program)
            .args(args)
            .current_dir(dir)
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        (out.status.success() && !text.trim().is_empty()).then(|| text.trim().to_string())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn copy(&mut self, from: &Path, to: &Path) -> bool {
        std::fs::copy(from, to).is_ok()
    }

    fn mkdir(&mut self, path: &Path) -> bool {
        std::fs::create_dir_all(path).is_ok()
    }

    fn touch(&mut self, path: &Path) {
        let _ = std::fs::write(path, "");
    }

    fn remove(&mut self, path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

/// Where a job runs and what it compares against.
struct Ctx<'a> {
    root: &'a Path,
    exe: &'a Path,
    scratch: &'a Scratch,
    /// The commit the head is compared with; none benches the head alone.
    base: Option<&'a str>,
    /// Where the merge base's worktree goes.
    tree: PathBuf,
}

/// Gates one job and returns its exit code: 0 passed, 1 regressed, anything
/// else errored. Afterwards it lists the items of a run that saved none.
fn gate(host: &mut impl Host, ctx: &Ctx, job: &Job) -> i32 {
    for stale in [
        ctx.scratch.baseline(&job.name),
        ctx.scratch.base_results(&job.name),
        ctx.scratch.save_failed(&job.name),
    ] {
        host.remove(&stale);
    }
    let code = match ctx.base {
        Some(base) => against(host, ctx, job, base),
        None => alone(host, ctx, job),
    };
    list_unsaved(host, ctx, job);
    code
}

fn head_dir(ctx: &Ctx, job: &Job) -> PathBuf {
    ctx.root.join(&job.dir)
}

fn bench(host: &mut impl Host, dir: &Path, job: &Job) -> bool {
    let args = ["-ec".to_string(), job.bench_script()];
    host.run(dir, "bash".as_ref(), &args) == 0
}

fn save(host: &mut impl Host, ctx: &Ctx, job: &Job) -> bool {
    let file = ctx.scratch.baseline(&job.name);
    let made = file.parent().is_none_or(|dir| host.mkdir(dir));
    made && host.run(&head_dir(ctx, job), ctx.exe, &job.save_args(&file)) == 0
}

/// The gate passed, so its items are saved for the docs job. A save that
/// fails is an error, since the docs job would then read stale numbers.
fn passed(host: &mut impl Host, ctx: &Ctx, job: &Job) -> i32 {
    if save(host, ctx, job) {
        return 0;
    }
    host.touch(&ctx.scratch.save_failed(&job.name));
    println!(
        "::error::save failed: the import gate passed, but HEAD's imported items could not be saved (a save or config error, not a gate failure)"
    );
    ERRORED
}

fn alone(host: &mut impl Host, ctx: &Ctx, job: &Job) -> i32 {
    if !bench(host, &head_dir(ctx, job), job) {
        return ERRORED;
    }
    passed(host, ctx, job)
}

fn against(host: &mut impl Host, ctx: &Ctx, job: &Job, base: &str) -> i32 {
    let find = ["merge-base".into(), "HEAD".into(), base.into()];
    let Some(merge_base) = host.output(ctx.root, "git".as_ref(), &find) else {
        return ERRORED;
    };
    let tree = ctx.tree.display().to_string();
    let add = ["worktree".into(), "add".into(), tree.clone(), merge_base];
    if host.run(ctx.root, "git".as_ref(), &add) != 0 {
        return ERRORED;
    }
    let code = compare(host, ctx, job);
    let remove = ["worktree", "remove", "--force", &tree].map(String::from);
    host.run(ctx.root, "git".as_ref(), &remove);
    code
}

fn compare(host: &mut impl Host, ctx: &Ctx, job: &Job) -> i32 {
    let head = head_dir(ctx, job);
    let base = match job.dir.as_str() {
        "." => ctx.tree.clone(),
        dir => ctx.tree.join(dir),
    };
    let base_out = base.join(&job.out);
    let head_ok = bench(host, &head, job) && host.exists(&head.join(&job.out));
    let base_benched = bench(host, &base, job);
    if host.exists(&base_out) {
        host.copy(&base_out, &ctx.scratch.base_results(&job.name));
    }
    let base_ok = base_benched && host.exists(&base_out);
    match sides(head_ok, base_ok) {
        Sides::HeadFailed => ERRORED,
        Sides::NothingToCompare => {
            println!("merge-base has no benchmark results; nothing to compare");
            println!("::notice::merge-base has no benchmark results; nothing to compare");
            passed(host, ctx, job)
        }
        Sides::Compare => {
            let code = host.run(&head, ctx.exe, &job.gate_args(&base_out));
            match gated(code) {
                Gated::Passed => passed(host, ctx, job),
                Gated::Regressed => {
                    if !save(host, ctx, job) {
                        println!("::error::could not save HEAD's imported items");
                    }
                    code
                }
                Gated::Errored(code) => code,
            }
        }
    }
}

fn list_unsaved(host: &mut impl Host, ctx: &Ctx, job: &Job) {
    let file = ctx.scratch.baseline(&job.name);
    if host.exists(&file) || file.parent().is_some_and(|dir| !host.mkdir(dir)) {
        return;
    }
    let base_results = ctx.scratch.base_results(&job.name);
    let kept = host.exists(&base_results).then_some(base_results.as_path());
    let marker = host.exists(&ctx.scratch.save_failed(&job.name));
    let args = job.failed_args(&file, kept, marker);
    host.run(&head_dir(ctx, job), ctx.exe, &args);
}

#[cfg(test)]
mod tests;
