//! `cargo soothfast plan`: what the reusable workflow runs for this repo,
//! read from `soothfast.toml` and the cargo workspace at the root.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::init::ci::Ci;
use crate::init::ecosystem::{Project, Suite, slug};
use crate::init::{self, RustPlan};
use crate::invoke;
use crate::test::project::{
    Pins, TEST_ALL_LABEL, Verdict, changed_since, covered, suites_for, target_for, toolchain_for,
    touches, write_result,
};

/// What the command line asked for beyond `--github`.
#[derive(Default)]
struct Args {
    github: bool,
    base: Option<String>,
    labels: Vec<String>,
    results: Option<PathBuf>,
    pins: Pins,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--github" {
            out.github = true;
            continue;
        }
        let value = it
            .next()
            .ok_or_else(|| format!("{a} needs a value"))?
            .clone();
        match a.as_str() {
            "--base" => out.base = Some(value),
            "--labels" => {
                out.labels = value
                    .split(',')
                    .filter(|l| !l.is_empty())
                    .map(String::from)
                    .collect()
            }
            "--results" => out.results = Some(PathBuf::from(value)),
            "--java" => out.pins.java = Some(value),
            "--node" => out.pins.node = Some(value),
            other => return Err(format!("unknown plan arg {other:?}")),
        }
    }
    Ok(out)
}

pub fn run(args: &[String]) -> i32 {
    let args = match parse_args(args) {
        Ok(args) => args,
        Err(e) => return err(&e),
    };
    let root = invoke::project_root();
    let plan = match build(&root, args.base.as_deref(), args.labels, args.pins) {
        Ok(plan) => plan,
        Err(e) => return err(&e),
    };
    if let Some(dir) = &args.results
        && let Err(e) = plan.write_settled(dir)
    {
        return err(&e);
    }
    if args.github {
        print!("{}", plan.outputs());
    } else {
        println!("{}", plan.matrix(true));
    }
    0
}

struct Plan {
    rust: Option<RustPlan>,
    projects: Vec<Project>,
    ci: Ci,
    docs: bool,
    suites: Vec<Suite>,
    /// Labels a writer applied to the pull request.
    labels: Vec<String>,
    /// The files the change touches; none selects every project.
    changed: Option<BTreeSet<String>>,
    pins: Pins,
    /// Names of the tests a stored pass already covers.
    reused: BTreeSet<String>,
}

/// One project's tests, or one suite's, and whether the change reaches them.
struct Candidate<'a> {
    name: String,
    project: &'a Project,
    suite: Option<&'a Suite>,
    reached: bool,
}

fn build(
    root: &Path,
    base: Option<&str>,
    labels: Vec<String>,
    mut pins: Pins,
) -> Result<Plan, String> {
    let rust = root
        .join("Cargo.toml")
        .is_file()
        .then(|| init::rust_plan(root))
        .transpose()?;
    let projects = init::manifest(root)?;
    let docs = match &rust {
        Some(rust) => !rust.docs_packages.is_empty(),
        None => !projects.is_empty() && init::has_markers(root),
    };
    let suites = init::suites(root)?;
    if let Some(suite) = suites
        .iter()
        .find(|s| !projects.iter().any(|p| p.dir == s.project))
    {
        return Err(format!(
            "suite {:?} names {}, which is not a [[project]]",
            suite.label, suite.project
        ));
    }
    let changed = match base {
        Some(base) => changed_since(root, Some(base)).map_err(|e| e.to_string())?,
        None => None,
    };
    let ci = init::ci(root)?;
    if pins.java.is_some() || pins.node.is_some() {
        pins.runner = Some(ci.runs_on_json());
    }
    let mut plan = Plan {
        rust,
        projects,
        ci,
        docs,
        suites,
        labels,
        changed,
        pins,
        reused: BTreeSet::new(),
    };
    plan.check_names()?;
    plan.reused = plan.stored_passes(root)?;
    Ok(plan)
}

impl Plan {
    /// The matrix of entries, one per project and, with `rust`, one for the
    /// cargo workspace.
    fn matrix(&self, rust: bool) -> Value {
        let mut include = Vec::new();
        if let (true, Some(plan)) = (rust, &self.rust) {
            include.push(json!({
                "name": "rust",
                "ecosystem": "rust",
                "dir": ".",
                "packages": plan.packages.join(" "),
            }));
        }
        for (project, name) in self.projects.iter().zip(names(&self.projects)) {
            let Some(bench) = &project.bench else {
                continue;
            };
            include.push(json!({
                "name": name,
                "ecosystem": project.ecosystem.name(),
                "dir": project.dir,
                "bench_cmd": bench.cmd,
                "bench_out": bench.out,
                "from": bench.from,
                "walltime": project.walltime.clone().unwrap_or_default(),
                "prefix": project.prefix.clone().unwrap_or_default(),
            }));
        }
        json!({ "include": include })
    }

    fn test_all(&self) -> bool {
        self.labels.iter().any(|l| l == TEST_ALL_LABEL)
    }

    /// The project tests and labelled suites, each with whether the change
    /// reaches it. `test-all` reaches every project.
    fn candidates(&self) -> Vec<Candidate<'_>> {
        let mut out = Vec::new();
        if !self.ci.runs_test() {
            return out;
        }
        for (project, name) in self.projects.iter().zip(names(&self.projects)) {
            if project.test.cmd.is_some() {
                let reached = self.test_all()
                    || self
                        .changed
                        .as_ref()
                        .is_none_or(|c| touches(&project.dir, &project.test.watch, c));
                out.push(Candidate {
                    name: name.clone(),
                    project,
                    suite: None,
                    reached,
                });
            }
            for suite in suites_for(&self.suites, &self.labels) {
                if suite.project == project.dir {
                    out.push(Candidate {
                        name: format!("{name}-{}", slug(&suite.label)),
                        project,
                        suite: Some(suite),
                        reached: true,
                    });
                }
            }
        }
        out
    }

    /// Each job and artifact is named after its test, so two cannot share
    /// a name.
    fn check_names(&self) -> Result<(), String> {
        let mut seen = BTreeSet::new();
        for (project, name) in self.projects.iter().zip(names(&self.projects)) {
            let suites = self.suites.iter().filter(|s| s.project == project.dir);
            let suites = suites.map(|s| format!("{name}-{}", slug(&s.label)));
            let own = project.test.cmd.is_some().then(|| name.clone());
            for test in own.into_iter().chain(suites) {
                if !seen.insert(test.clone()) {
                    return Err(format!(
                        "two tests are named {test:?}; rename a project or a suite label"
                    ));
                }
            }
        }
        Ok(())
    }

    /// The names of reached tests that a stored pass covers. `test-all`
    /// ignores the store.
    fn stored_passes(&self, root: &Path) -> Result<BTreeSet<String>, String> {
        if self.test_all() {
            return Ok(BTreeSet::new());
        }
        let mut tests = Vec::new();
        for c in self.candidates().into_iter().filter(|c| c.reached) {
            let target = target_for(c.project, c.suite)?;
            let toolchain = toolchain_for(c.project.ecosystem, &self.pins);
            tests.push((c.name, target, toolchain));
        }
        covered(root, &tests).map_err(|e| e.to_string())
    }

    /// One entry per reached test with no stored pass: the only tests a job
    /// has to run.
    fn test_matrix(&self) -> Value {
        let include: Vec<Value> = self
            .candidates()
            .into_iter()
            .filter(|c| c.reached && !self.reused.contains(&c.name))
            .map(|c| {
                json!({
                    "name": c.name,
                    "ecosystem": c.project.ecosystem.name(),
                    "dir": c.project.dir,
                    "suite": c.suite.map(|s| s.label.as_str()).unwrap_or_default(),
                    "toolchain": toolchain_for(c.project.ecosystem, &self.pins),
                })
            })
            .collect();
        json!({ "include": include })
    }

    /// The tests that need no job, and why.
    fn settled(&self) -> Vec<(String, String, Verdict)> {
        self.candidates()
            .into_iter()
            .filter_map(|c| {
                let verdict = match (c.reached, self.reused.contains(&c.name)) {
                    (false, _) => Verdict::Skipped,
                    (true, true) => Verdict::Reused,
                    (true, false) => return None,
                };
                Some((c.name, c.project.dir.clone(), verdict))
            })
            .collect()
    }

    /// A result file in `dir` for each test that needs no job.
    fn write_settled(&self, dir: &Path) -> Result<(), String> {
        let settled = self.settled();
        if settled.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for (name, project, verdict) in settled {
            let path = dir.join(format!("{name}.json"));
            write_result(&path, &name, &project, verdict, &[], 0.0)?;
        }
        Ok(())
    }

    /// `key=value` lines for `$GITHUB_OUTPUT`. The matrix holds only the
    /// non-Rust projects: the cargo workspace gates through its own job.
    fn outputs(&self) -> String {
        let docs_package = self
            .rust
            .as_ref()
            .map(|r| r.docs_packages.join(" "))
            .unwrap_or_default();
        let packages = self
            .rust
            .as_ref()
            .map(|r| r.packages.join(" "))
            .unwrap_or_default();
        let tests = self.test_matrix();
        format!(
            "matrix={}\nprojects={}\nrust={}\ntest={}\npackages={packages}\ndocs={}\ndocs_package={docs_package}\ntest_matrix={}\ntest_projects={}\ntest_all={}\nruns_on={}\n",
            self.matrix(false),
            self.projects.iter().any(|p| p.bench.is_some()),
            self.rust.is_some(),
            self.rust.is_some() && self.ci.runs_test(),
            self.docs,
            tests,
            !tests["include"].as_array().is_none_or(Vec::is_empty),
            self.test_all(),
            self.ci.runs_on_json(),
        )
    }
}

/// Each project's name: its ecosystem, plus its directory when another
/// project shares the ecosystem.
fn names(projects: &[Project]) -> Vec<String> {
    projects
        .iter()
        .map(|p| {
            let name = p.ecosystem.name();
            if projects
                .iter()
                .filter(|q| q.ecosystem == p.ecosystem)
                .count()
                == 1
            {
                return name.to_string();
            }
            match p.dir.as_str() {
                "." => format!("{name}-root"),
                dir => format!(
                    "{name}-{}",
                    dir.replace(|c: char| !c.is_ascii_alphanumeric(), "-")
                ),
            }
        })
        .collect()
}

fn err(msg: &str) -> i32 {
    eprintln!("plan: {msg}");
    2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::ecosystem::{Bench, Ecosystem, Tests};

    fn project(ecosystem: Ecosystem, dir: &str) -> Project {
        Project {
            ecosystem,
            dir: dir.into(),
            bench: Some(Bench {
                cmd: "run {out}".into(),
                out: "soothfast-bench.json".into(),
                from: "jmh".into(),
            }),
            walltime: None,
            prefix: None,
            test: Tests::default(),
        }
    }

    fn plan(rust: Option<&[&str]>, projects: Vec<Project>) -> Plan {
        Plan {
            rust: rust.map(|packages| RustPlan {
                packages: packages.iter().map(|p| p.to_string()).collect(),
                docs_packages: Vec::new(),
            }),
            projects,
            ci: Ci::default(),
            docs: false,
            suites: Vec::new(),
            labels: Vec::new(),
            changed: None,
            pins: Pins {
                runner: Some("Linux X64".into()),
                ..Pins::default()
            },
            reused: BTreeSet::new(),
        }
    }

    #[test]
    fn the_matrix_has_a_rust_entry_and_one_per_project() {
        let p = plan(
            Some(&["a", "b"]),
            vec![project(Ecosystem::Gradle, "client")],
        );
        let all = p.matrix(true);
        let entries = all["include"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["ecosystem"], "rust");
        assert_eq!(entries[0]["packages"], "a b");
        assert_eq!(entries[1]["name"], "gradle");
        assert_eq!(entries[1]["dir"], "client");
        assert_eq!(entries[1]["bench_cmd"], "run {out}");
    }

    #[test]
    fn the_github_outputs_keep_rust_out_of_the_project_matrix() {
        let p = plan(Some(&["a"]), vec![project(Ecosystem::Python, "py")]);
        let out = p.outputs();
        assert!(
            out.contains("projects=true\nrust=true\ntest=true\npackages=a\n"),
            "{out}"
        );
        let matrix = out.lines().next().unwrap().strip_prefix("matrix=").unwrap();
        let matrix: Value = serde_json::from_str(matrix).unwrap();
        assert_eq!(matrix["include"].as_array().unwrap().len(), 1);
        let pure = plan(None, vec![]).outputs();
        assert!(pure.contains("projects=false\nrust=false\n"), "{pure}");
    }

    #[test]
    fn the_ci_table_picks_the_runners_and_drops_the_test_job() {
        let mut p = plan(Some(&["a"]), vec![]);
        p.ci = Ci::parse("[runner]\nlabels = [\"self-hosted\", \"linux\"]\n\n[test]\nci = false\n")
            .unwrap();
        let out = p.outputs();
        assert!(out.contains("rust=true\ntest=false\n"), "{out}");
        assert!(
            out.ends_with("runs_on=[\"self-hosted\",\"linux\"]\n"),
            "{out}"
        );
    }

    fn testable(ecosystem: Ecosystem, dir: &str, watch: &[&str]) -> Project {
        let mut p = project(ecosystem, dir);
        p.test = Tests {
            cmd: Some("run tests".into()),
            watch: watch.iter().map(|g| g.to_string()).collect(),
            ..Tests::default()
        };
        p
    }

    fn test_names(p: &Plan) -> Vec<String> {
        let matrix = p.test_matrix();
        let entries = matrix["include"].as_array().unwrap();
        entries
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn the_test_matrix_holds_the_projects_the_change_reaches() {
        let mut p = plan(
            None,
            vec![
                testable(Ecosystem::Gradle, "client", &["gradle/libs.versions.toml"]),
                testable(Ecosystem::Node, "web", &[]),
                project(Ecosystem::Python, "py"),
            ],
        );
        assert_eq!(test_names(&p), ["gradle", "node"]);
        p.changed = Some(["web/a.js".to_string()].into());
        assert_eq!(test_names(&p), ["node"]);
        p.changed = Some(["gradle/libs.versions.toml".to_string()].into());
        assert_eq!(test_names(&p), ["gradle"]);
        p.changed = Some(BTreeSet::new());
        assert!(test_names(&p).is_empty());
        p.labels = vec![TEST_ALL_LABEL.into()];
        assert!(p.test_all());
        assert_eq!(test_names(&p), ["gradle", "node"]);
        assert!(
            p.outputs()
                .contains("test_projects=true\ntest_all=true\nruns_on")
        );
    }

    #[test]
    fn a_test_only_project_stays_out_of_the_gate_matrix() {
        let mut only = testable(Ecosystem::Python, "py", &[]);
        only.bench = None;
        let p = plan(None, vec![only]);
        let out = p.outputs();
        assert!(out.contains("projects=false\n"), "{out}");
        assert!(p.matrix(false)["include"].as_array().unwrap().is_empty());
        assert_eq!(test_names(&p), ["python"]);
    }

    #[test]
    fn a_reused_or_untouched_test_needs_no_job() {
        let mut p = plan(
            None,
            vec![
                testable(Ecosystem::Gradle, "client", &[]),
                testable(Ecosystem::Node, "web", &[]),
            ],
        );
        p.changed = Some(["client/A.java".to_string()].into());
        let matrix = p.test_matrix();
        assert_eq!(matrix["include"][0]["toolchain"], "gradle  Linux X64");
        assert_eq!(test_names(&p), ["gradle"]);
        p.reused.insert("gradle".into());
        assert!(test_names(&p).is_empty());
        let settled = p.settled();
        let verdicts: Vec<_> = settled.iter().map(|(n, _, v)| (n.as_str(), *v)).collect();
        assert_eq!(
            verdicts,
            [("gradle", Verdict::Reused), ("node", Verdict::Skipped)]
        );
        assert!(p.outputs().contains("test_projects=false\n"));
    }

    #[test]
    fn settled_results_are_written_one_file_each() {
        let mut p = plan(None, vec![testable(Ecosystem::Node, "web", &[])]);
        p.changed = Some(BTreeSet::new());
        let dir = std::env::temp_dir().join(format!("soothfast-settled-{}", std::process::id()));
        p.write_settled(&dir).unwrap();
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("node.json")).unwrap()).unwrap();
        assert_eq!(doc["verdict"], "skipped");
        assert_eq!(doc["kind"], "test");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plan_flags_are_parsed() {
        let argv = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let a = parse_args(&argv(&["--github", "--labels", "a,b", "--java", "21"])).unwrap();
        assert!(a.github);
        assert_eq!(a.labels, ["a", "b"]);
        assert_eq!(a.pins.java.as_deref(), Some("21"));
        assert!(parse_args(&argv(&["--base"])).is_err());
        assert!(parse_args(&argv(&["--nope", "x"])).is_err());
    }

    #[test]
    fn ci_false_drops_the_project_tests_too() {
        let mut p = plan(None, vec![testable(Ecosystem::Node, "web", &[])]);
        assert_eq!(test_names(&p), ["node"]);
        p.ci = Ci::parse("[test]\nci = false\n").unwrap();
        assert!(test_names(&p).is_empty());
        assert!(p.settled().is_empty());
        assert!(p.outputs().contains("test_projects=false\n"));
    }

    #[test]
    fn two_tests_with_one_name_are_rejected() {
        let mut p = plan(
            None,
            vec![
                testable(Ecosystem::Node, "a", &[]),
                testable(Ecosystem::Node, "a-b", &[]),
            ],
        );
        assert!(p.check_names().is_ok());
        p.suites = vec![Suite {
            label: "b".into(),
            project: "a".into(),
            cmd: "c".into(),
        }];
        let err = p.check_names().unwrap_err();
        assert!(err.contains("two tests are named"), "{err}");
    }

    #[test]
    fn a_suite_joins_the_matrix_when_its_label_is_applied() {
        let mut p = plan(None, vec![testable(Ecosystem::Node, "web", &[])]);
        p.suites = vec![Suite {
            label: "soothfast:e2e".into(),
            project: "web".into(),
            cmd: "npm run e2e".into(),
        }];
        p.changed = Some(BTreeSet::new());
        assert!(test_names(&p).is_empty());
        assert!(p.outputs().contains("test_projects=false\n"));
        p.labels = vec!["soothfast:e2e".into()];
        assert_eq!(test_names(&p), ["node-soothfast-e2e"]);
        assert_eq!(p.test_matrix()["include"][0]["suite"], "soothfast:e2e");
    }

    #[test]
    fn projects_sharing_an_ecosystem_are_named_by_directory() {
        let projects = vec![
            project(Ecosystem::Python, "."),
            project(Ecosystem::Python, "plugin/x"),
            project(Ecosystem::Node, "web"),
        ];
        assert_eq!(names(&projects), ["python-root", "python-plugin-x", "node"]);
    }
}
