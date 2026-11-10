//! `ci gate-import`: the perf gate of a project outside the cargo workspace.
//! It benches the base and the head with the project's own harness and gates
//! the imported results.

use std::path::{Path, PathBuf};

use crate::init::ecosystem::{Ecosystem, Project};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::ecosystem::{Bench, Tests};

    fn project(ecosystem: Ecosystem, dir: &str, bench: bool) -> Project {
        Project {
            ecosystem,
            dir: dir.into(),
            bench: bench.then(|| Bench {
                cmd: "run --json {out}".into(),
                out: "bench.json".into(),
                from: "jmh".into(),
            }),
            walltime: Some("hard".into()),
            prefix: None,
            test: Tests::default(),
        }
    }

    fn job(ecosystem: Ecosystem) -> Job {
        jobs(&[project(ecosystem, ".", true)], None, None)
            .unwrap()
            .remove(0)
    }

    #[test]
    fn a_head_that_failed_decides_before_the_base_is_read() {
        assert_eq!(sides(false, true), Sides::HeadFailed);
        assert_eq!(sides(false, false), Sides::HeadFailed);
    }

    #[test]
    fn a_base_without_results_has_nothing_to_compare() {
        assert_eq!(sides(true, false), Sides::NothingToCompare);
    }

    #[test]
    fn two_sides_with_results_are_compared() {
        assert_eq!(sides(true, true), Sides::Compare);
    }

    #[test]
    fn the_gate_exit_code_is_a_pass_a_regression_or_an_error() {
        assert_eq!(gated(0), Gated::Passed);
        assert_eq!(gated(1), Gated::Regressed);
        assert_eq!(gated(2), Gated::Errored(2));
        assert_eq!(gated(101), Gated::Errored(101));
    }

    #[test]
    fn the_bench_writes_to_the_out_path() {
        assert_eq!(
            job(Ecosystem::Gradle).bench_script(),
            "run --json bench.json"
        );
    }

    #[test]
    fn a_node_bench_installs_first() {
        assert_eq!(
            job(Ecosystem::Node).bench_script(),
            "npm ci\nrun --json bench.json"
        );
    }

    #[test]
    fn the_gate_compares_the_head_results_to_the_base_results() {
        let mut job = job(Ecosystem::Gradle);
        job.prefix = Some("web".into());
        assert_eq!(
            job.gate_args(Path::new("/t/base/bench.json")),
            [
                "gate",
                "--import",
                "jmh",
                "bench.json",
                "--reference-import",
                "jmh",
                "/t/base/bench.json",
                "--prefix",
                "web",
                "--walltime",
                "hard",
            ]
        );
    }

    #[test]
    fn the_save_and_the_failed_listing_name_the_same_file() {
        let job = job(Ecosystem::Gradle);
        let scratch = Scratch("/t".into());
        let file = scratch.baseline("gradle");
        assert_eq!(file, Path::new("/t/soothfast-baseline/import-gradle.json"));
        assert_eq!(
            job.save_args(&file),
            [
                "import",
                "--from",
                "jmh",
                "bench.json",
                "--out",
                "/t/soothfast-baseline/import-gradle.json"
            ]
        );
        let base = scratch.base_results("gradle");
        assert_eq!(
            job.failed_args(&file, Some(&base), true),
            [
                "import",
                "--from",
                "jmh",
                "bench.json",
                "--out",
                "/t/soothfast-baseline/import-gradle.json",
                "--failed",
                "--base-results",
                "/t/soothfast-base-results-gradle",
                "--save-failed",
            ]
        );
        let bare = job.failed_args(&file, None, false);
        assert_eq!(bare.last().map(String::as_str), Some("--failed"));
    }

    #[test]
    fn every_project_that_benches_is_a_job_named_as_the_matrix_names_it() {
        let projects = [
            project(Ecosystem::Gradle, "a", true),
            project(Ecosystem::Gradle, "b/c", true),
            project(Ecosystem::Node, "web", false),
            project(Ecosystem::Python, ".", true),
        ];
        let names: Vec<_> = jobs(&projects, None, None)
            .unwrap()
            .into_iter()
            .map(|j| j.name)
            .collect();
        assert_eq!(names, ["gradle-a", "gradle-b-c", "python"]);
    }

    #[test]
    fn a_project_flag_picks_one_and_a_name_flag_renames_it() {
        let projects = [
            project(Ecosystem::Gradle, "a", true),
            project(Ecosystem::Gradle, "b", true),
        ];
        let picked = jobs(&projects, Some("b"), Some("perf-b")).unwrap();
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].name, "perf-b");
        assert_eq!(picked[0].dir, "b");
    }

    #[test]
    fn a_project_without_a_benchmark_command_is_an_error() {
        let projects = [project(Ecosystem::Node, "web", false)];
        assert!(jobs(&projects, Some("web"), None).is_err());
        assert!(jobs(&projects, Some("other"), None).is_err());
        assert!(jobs(&projects, None, None).unwrap().is_empty());
    }
}
