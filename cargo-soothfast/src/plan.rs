//! `cargo soothfast plan`: what the reusable workflow runs for this repo,
//! read from `soothfast.toml` and the cargo workspace at the root.

use std::path::Path;

use serde_json::{Value, json};

use crate::init::ci::Ci;
use crate::init::ecosystem::Project;
use crate::init::{self, RustPlan};
use crate::invoke;

pub fn run(args: &[String]) -> i32 {
    let mut github = false;
    for a in args {
        match a.as_str() {
            "--github" => github = true,
            other => return err(&format!("unknown plan arg {other:?}")),
        }
    }
    let root = invoke::project_root();
    let plan = match build(&root) {
        Ok(plan) => plan,
        Err(e) => return err(&e),
    };
    if github {
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
}

fn build(root: &Path) -> Result<Plan, String> {
    let rust = root
        .join("Cargo.toml")
        .is_file()
        .then(|| init::rust_plan(root))
        .transpose()?;
    let projects = init::manifest(root)?;
    let docs = match &rust {
        Some(rust) => rust.docs_package.is_some(),
        None => !projects.is_empty() && init::has_markers(root),
    };
    Ok(Plan {
        rust,
        projects,
        ci: init::ci(root)?,
        docs,
    })
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
            include.push(json!({
                "name": name,
                "ecosystem": project.ecosystem.name(),
                "dir": project.dir,
                "bench_cmd": project.bench.cmd,
                "bench_out": project.bench.out,
                "from": project.bench.from,
                "walltime": project.walltime.clone().unwrap_or_default(),
                "prefix": project.prefix.clone().unwrap_or_default(),
            }));
        }
        json!({ "include": include })
    }

    /// `key=value` lines for `$GITHUB_OUTPUT`. The matrix holds only the
    /// non-Rust projects: the cargo workspace gates through its own job.
    fn outputs(&self) -> String {
        let docs_package = self
            .rust
            .as_ref()
            .and_then(|r| r.docs_package.clone())
            .unwrap_or_default();
        let packages = self
            .rust
            .as_ref()
            .map(|r| r.packages.join(" "))
            .unwrap_or_default();
        format!(
            "matrix={}\nprojects={}\nrust={}\ntest={}\npackages={packages}\ndocs={}\ndocs_package={docs_package}\nruns_on={}\n",
            self.matrix(false),
            !self.projects.is_empty(),
            self.rust.is_some(),
            self.rust.is_some() && self.ci.runs_test(),
            self.docs,
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
    use crate::init::ecosystem::{Bench, Ecosystem};

    fn project(ecosystem: Ecosystem, dir: &str) -> Project {
        Project {
            ecosystem,
            dir: dir.into(),
            bench: Bench {
                cmd: "run {out}".into(),
                out: "soothfast-bench.json".into(),
                from: "jmh".into(),
            },
            walltime: None,
            prefix: None,
        }
    }

    fn plan(rust: Option<&[&str]>, projects: Vec<Project>) -> Plan {
        Plan {
            rust: rust.map(|packages| RustPlan {
                packages: packages.iter().map(|p| p.to_string()).collect(),
                docs_package: None,
            }),
            projects,
            ci: Ci::default(),
            docs: false,
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
        p.ci = Ci::parse("[ci]\nruns_on = [\"self-hosted\", \"linux\"]\ntest = false\n").unwrap();
        let out = p.outputs();
        assert!(out.contains("rust=true\ntest=false\n"), "{out}");
        assert!(
            out.ends_with("runs_on=[\"self-hosted\",\"linux\"]\n"),
            "{out}"
        );
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
