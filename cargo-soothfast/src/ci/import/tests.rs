use super::*;
use crate::ci::env::Flags;
use crate::init::ecosystem::{Bench, Tests};
use std::collections::{BTreeMap, BTreeSet};

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

fn argv(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn ci_env(event: Event, base: Option<&str>, results: Option<&Path>) -> CiEnv {
    let flags = Flags {
        event: Some(event),
        base: base.map(String::from),
        results: results.map(Path::to_path_buf),
    };
    CiEnv::parse(&BTreeMap::new(), None, &flags)
}

#[test]
fn the_flags_are_a_project_a_name_and_a_label() {
    let args = parse(&argv(&[
        "--project",
        "./web/",
        "--name",
        "perf-web",
        "--accepted",
        "ship-it",
    ]))
    .unwrap();
    assert_eq!(args.project.as_deref(), Some("web"));
    assert_eq!(args.name.as_deref(), Some("perf-web"));
    assert_eq!(args.accepted.as_deref(), Some("ship-it"));
    assert_eq!(parse(&[]).unwrap(), Args::default());
}

#[test]
fn a_bad_flag_is_an_error() {
    assert!(parse(&argv(&["--name", "x"])).is_err());
    assert!(parse(&argv(&["--inner"])).is_err());
    assert!(parse(&argv(&["--project"])).is_err());
    assert!(parse(&argv(&["--project", "../up"])).is_err());
    assert!(parse(&argv(&["--nope"])).is_err());
}

#[test]
fn a_regression_is_accepted_by_a_label_and_an_error_never() {
    assert_eq!(settle(1, Some("ship-it")), 0);
    assert_eq!(settle(1, None), 1);
    assert_eq!(settle(3, Some("ship-it")), 3);
    assert_eq!(settle(2, Some("ship-it")), 2);
    assert_eq!(settle(0, Some("ship-it")), 0);
}

#[test]
fn every_project_is_gated_and_the_exit_code_is_the_worst() {
    let projects = [
        project(Ecosystem::Gradle, "a", true),
        project(Ecosystem::Node, "b", true),
        project(Ecosystem::Python, "c", true),
    ];
    let jobs = jobs(&projects, None, None).unwrap();
    let mut gated = Vec::new();
    let code = gate_all(&jobs, None, |job| {
        gated.push(job.name.clone());
        [1, 3, 0][gated.len() - 1]
    });
    assert_eq!(code, 3);
    assert_eq!(gated, ["gradle", "node", "python"]);
}

#[test]
fn a_waived_regression_does_not_hide_a_later_error() {
    let projects = [
        project(Ecosystem::Gradle, "a", true),
        project(Ecosystem::Node, "b", true),
    ];
    let jobs = jobs(&projects, None, None).unwrap();
    let mut codes = [1, 3].into_iter();
    assert_eq!(gate_all(&jobs, Some("ok"), |_| codes.next().unwrap()), 3);
    let mut codes = [1, 0].into_iter();
    assert_eq!(gate_all(&jobs, Some("ok"), |_| codes.next().unwrap()), 0);
    assert_eq!(gate_all(&[], None, |_| 1), 0);
}

#[test]
fn a_job_reruns_itself_with_the_run_it_is_in() {
    let job = job(Ecosystem::Gradle);
    let env = ci_env(Event::PullRequest, Some("b4se"), None);
    assert_eq!(
        inner_args(&env, &job),
        [
            "ci",
            "--event",
            "pull_request",
            "--base",
            "b4se",
            "gate-import",
            "--project",
            ".",
            "--name",
            "gradle",
            "--inner"
        ]
    );
    let push = ci_env(Event::Push, None, None);
    assert_eq!(&inner_args(&push, &job)[..3], ["ci", "--event", "push"]);
    assert!(!inner_args(&push, &job).contains(&"--base".into()));
}

#[test]
fn only_a_pull_request_compares_with_a_base() {
    let base = Some("b4se");
    assert_eq!(compare_base(&ci_env(Event::PullRequest, base, None)), base);
    assert_eq!(compare_base(&ci_env(Event::Push, base, None)), None);
    assert_eq!(compare_base(&ci_env(Event::Other, base, None)), None);
    assert_eq!(compare_base(&ci_env(Event::PullRequest, None, None)), None);
}

#[test]
fn a_project_leaves_one_record_named_for_it_of_kind_import() {
    let dir = std::env::temp_dir().join(format!("soothfast-gate-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("fake-soothfast");
    std::fs::write(&exe, "#!/bin/sh\necho \"regressed: $*\"\nexit 1\n").unwrap();
    std::fs::set_permissions(&exe, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let results = dir.join("results");
    let env = ci_env(Event::PullRequest, Some("b4se"), Some(&results));
    assert_eq!(launch(&env, &exe, &job(Ecosystem::Gradle)), 1);
    let text = std::fs::read_to_string(results.join("gradle.json")).unwrap();
    let record: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(record["name"], "gradle");
    assert_eq!(record["kind"], "import");
    assert_eq!(record["verdict"], "regressed");
    assert!(record["tail"].as_str().unwrap().contains("--inner"));
    let names: Vec<_> = std::fs::read_dir(&results)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 2);
    std::fs::remove_dir_all(dir).unwrap();
}

const HEAD_BENCH: &str = "/repo/p bash";
const BASE_BENCH: &str = "/t/tree/p bash";
const GATE: &str = "soothfast gate";
const SAVE: &str = "soothfast import --from jmh bench.json --out";
const BASELINE: &str = "/t/scratch/soothfast-baseline/import-gradle.json";

struct Fake {
    files: BTreeSet<PathBuf>,
    rules: Vec<(&'static str, i32, Vec<&'static str>)>,
    merge_base: Option<&'static str>,
    log: Vec<String>,
}

impl Fake {
    /// Both sides bench and leave results, and every import saves.
    fn healthy() -> Fake {
        Fake {
            files: BTreeSet::new(),
            rules: vec![
                (HEAD_BENCH, 0, vec!["/repo/p/bench.json"]),
                (BASE_BENCH, 0, vec!["/t/tree/p/bench.json"]),
                ("soothfast import", 0, vec![BASELINE]),
            ],
            merge_base: Some("m3rge"),
            log: Vec::new(),
        }
    }

    fn rule(mut self, pattern: &'static str, code: i32, writes: &[&'static str]) -> Fake {
        self.rules.insert(0, (pattern, code, writes.to_vec()));
        self
    }

    fn ran(&self, pattern: &str) -> bool {
        self.log.iter().any(|l| l.contains(pattern))
    }

    fn saved(&self) -> bool {
        self.log
            .iter()
            .any(|l| l.contains(SAVE) && !l.contains("--failed"))
    }
}

impl Host for Fake {
    fn run(&mut self, dir: &Path, program: &Path, args: &[String]) -> i32 {
        let line = format!("{} {} {}", dir.display(), program.display(), args.join(" "));
        self.log.push(line.clone());
        let Some((_, code, writes)) = self.rules.iter().find(|(p, ..)| line.contains(p)) else {
            return 0;
        };
        if *code == 0 {
            self.files.extend(writes.iter().map(PathBuf::from));
        }
        *code
    }

    fn output(&mut self, _: &Path, _: &Path, _: &[String]) -> Option<String> {
        self.merge_base.map(String::from)
    }

    fn exists(&self, path: &Path) -> bool {
        self.files.contains(path)
    }

    fn copy(&mut self, from: &Path, to: &Path) -> bool {
        self.files.contains(from) && self.files.insert(to.to_path_buf())
    }

    fn mkdir(&mut self, _: &Path) -> bool {
        true
    }

    fn touch(&mut self, path: &Path) {
        self.files.insert(path.to_path_buf());
    }

    fn remove(&mut self, path: &Path) {
        self.files.remove(path);
    }
}

fn gate_job(host: &mut Fake, base: Option<&str>) -> i32 {
    let scratch = Scratch("/t/scratch".into());
    let ctx = Ctx {
        root: Path::new("/repo"),
        exe: Path::new("soothfast"),
        scratch: &scratch,
        base,
        tree: "/t/tree".into(),
    };
    let job = jobs(&[project(Ecosystem::Gradle, "p", true)], None, None)
        .unwrap()
        .remove(0);
    gate(host, &ctx, &job)
}

#[test]
fn without_a_base_the_head_is_benched_and_saved() {
    let mut host = Fake::healthy();
    assert_eq!(gate_job(&mut host, None), 0);
    assert!(host.ran(HEAD_BENCH) && host.saved());
    assert!(!host.ran("worktree") && !host.ran(GATE) && !host.ran("--failed"));
}

#[test]
fn without_a_base_a_failed_bench_errors_and_lists_the_items() {
    let mut host = Fake::healthy().rule(HEAD_BENCH, 1, &[]);
    assert_eq!(gate_job(&mut host, None), ERRORED);
    assert!(!host.saved());
    assert!(host.ran("--failed") && !host.ran("--save-failed"));
}

#[test]
fn a_pass_that_cannot_save_errors_and_says_so_in_the_listing() {
    let mut host = Fake::healthy().rule(SAVE, 1, &[]);
    assert_eq!(gate_job(&mut host, None), ERRORED);
    assert!(host.ran("--failed") && host.ran("--save-failed"));
}

#[test]
fn a_base_is_benched_in_a_worktree_of_the_merge_base() {
    let mut host = Fake::healthy();
    assert_eq!(gate_job(&mut host, Some("b4se")), 0);
    assert!(host.ran("/repo git worktree add /t/tree m3rge"));
    assert!(host.ran(HEAD_BENCH) && host.ran(BASE_BENCH));
    assert!(host.ran(
        "/repo/p soothfast gate --import jmh bench.json --reference-import jmh /t/tree/p/bench.json --walltime hard"
    ));
    assert!(host.saved());
}

#[test]
fn the_worktree_is_removed_when_the_gate_ends() {
    let mut host = Fake::healthy();
    gate_job(&mut host, Some("b4se"));
    assert!(host.ran("/repo git worktree remove --force /t/tree"));
    assert!(!host.ran("prune"));
}

#[test]
fn the_worktree_is_removed_when_a_bench_fails() {
    let mut host = Fake::healthy()
        .rule(BASE_BENCH, 1, &[])
        .rule(HEAD_BENCH, 1, &[]);
    assert_eq!(gate_job(&mut host, Some("b4se")), ERRORED);
    assert!(host.ran("worktree remove --force /t/tree"));
}

#[test]
fn a_head_that_failed_still_benches_the_base_and_keeps_its_results() {
    let mut host = Fake::healthy().rule(HEAD_BENCH, 1, &[]);
    assert_eq!(gate_job(&mut host, Some("b4se")), ERRORED);
    assert!(host.ran(BASE_BENCH) && !host.ran(GATE) && !host.saved());
    assert!(host.ran("--failed --base-results /t/scratch/soothfast-base-results-gradle"));
}

#[test]
fn a_head_with_no_results_is_a_failure_even_when_its_bench_exits_zero() {
    let mut host = Fake::healthy().rule(HEAD_BENCH, 0, &[]);
    assert_eq!(gate_job(&mut host, Some("b4se")), ERRORED);
    assert!(host.ran(BASE_BENCH) && !host.ran(GATE));
}

#[test]
fn a_base_that_failed_has_nothing_to_compare_and_the_head_is_saved() {
    let mut host = Fake::healthy().rule(BASE_BENCH, 1, &[]);
    assert_eq!(gate_job(&mut host, Some("b4se")), 0);
    assert!(!host.ran(GATE) && host.saved());
    assert!(!host.ran("--failed"));
}

#[test]
fn a_base_with_no_results_has_nothing_to_compare() {
    let mut host = Fake::healthy().rule(BASE_BENCH, 0, &[]);
    assert_eq!(gate_job(&mut host, Some("b4se")), 0);
    assert!(!host.ran(GATE) && host.saved());
}

#[test]
fn a_base_that_failed_after_writing_results_still_has_them_kept() {
    let mut host = Fake::healthy().rule(BASE_BENCH, 1, &[]);
    host.files.insert("/t/tree/p/bench.json".into());
    gate_job(&mut host, Some("b4se"));
    assert!(host.exists(Path::new("/t/scratch/soothfast-base-results-gradle")));
}

#[test]
fn a_nothing_to_compare_pass_that_cannot_save_errors() {
    let mut host = Fake::healthy().rule(BASE_BENCH, 1, &[]).rule(SAVE, 1, &[]);
    assert_eq!(gate_job(&mut host, Some("b4se")), ERRORED);
    assert!(host.ran("--save-failed"));
}

#[test]
fn a_regression_exits_one_and_still_saves_the_head() {
    let mut host = Fake::healthy().rule(GATE, 1, &[]);
    assert_eq!(gate_job(&mut host, Some("b4se")), 1);
    assert!(host.saved() && !host.ran("--failed"));
}

#[test]
fn a_regression_whose_save_fails_is_still_a_regression() {
    let mut host = Fake::healthy().rule(GATE, 1, &[]).rule(SAVE, 1, &[]);
    assert_eq!(gate_job(&mut host, Some("b4se")), 1);
    assert!(host.ran("--failed"));
}

#[test]
fn a_gate_error_saves_nothing_and_keeps_its_code() {
    let mut host = Fake::healthy().rule(GATE, 2, &[]);
    assert_eq!(gate_job(&mut host, Some("b4se")), 2);
    assert!(!host.saved());
    assert!(host.ran("--failed"));
}

#[test]
fn a_worktree_that_cannot_be_made_errors_before_any_bench() {
    let mut host = Fake::healthy().rule("worktree add", 128, &[]);
    assert_eq!(gate_job(&mut host, Some("b4se")), ERRORED);
    assert!(!host.ran(HEAD_BENCH) && !host.ran("worktree remove"));
}

#[test]
fn a_missing_merge_base_errors_before_any_bench() {
    let mut host = Fake::healthy();
    host.merge_base = None;
    assert_eq!(gate_job(&mut host, Some("b4se")), ERRORED);
    assert!(!host.ran("worktree") && !host.ran(HEAD_BENCH));
}

#[test]
fn leftovers_of_an_earlier_run_do_not_hide_this_runs_failure() {
    let mut host = Fake::healthy().rule(HEAD_BENCH, 1, &[]);
    host.files.insert(BASELINE.into());
    host.files
        .insert("/t/scratch/soothfast-save-failed-gradle".into());
    gate_job(&mut host, None);
    assert!(host.ran("--failed") && !host.ran("--save-failed"));
}

#[test]
fn a_project_at_the_root_compares_the_worktree_itself() {
    let scratch = Scratch("/t/scratch".into());
    let ctx = Ctx {
        root: Path::new("/repo"),
        exe: Path::new("soothfast"),
        scratch: &scratch,
        base: Some("b4se"),
        tree: "/t/tree".into(),
    };
    let job = job(Ecosystem::Gradle);
    let mut host = Fake::healthy()
        .rule("/repo/. bash", 0, &["/repo/./bench.json"])
        .rule("/t/tree bash", 0, &["/t/tree/bench.json"]);
    assert_eq!(gate(&mut host, &ctx, &job), 0);
    assert!(host.ran("--reference-import jmh /t/tree/bench.json"));
}

#[test]
fn the_node_bench_is_one_script_run_by_bash() {
    let mut host = Fake::healthy();
    let scratch = Scratch("/t/scratch".into());
    let ctx = Ctx {
        root: Path::new("/repo"),
        exe: Path::new("soothfast"),
        scratch: &scratch,
        base: None,
        tree: "/t/tree".into(),
    };
    let job = jobs(&[project(Ecosystem::Node, "p", true)], None, None)
        .unwrap()
        .remove(0);
    gate(&mut host, &ctx, &job);
    assert!(host.ran("/repo/p bash -ec npm ci\nrun --json bench.json"));
}
