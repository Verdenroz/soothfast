//! What a lane reads, and the fingerprint of it.
//!
//! A fingerprint errs wide: an input it holds too many of costs a lane that
//! could have been carried, one it lacks carries a pass that no longer
//! stands.

use std::collections::{BTreeMap, BTreeSet};

use super::graph::Graph;
use crate::init::ecosystem::Project;
use crate::runcache::hashed;
use crate::test::config::TestConfig;
use crate::test::project::{Pins, toolchain_for};
use crate::test::select::{Scope, classify};
use crate::{binstore, glob};

/// A set of files a lane reads.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reads {
    /// The files that build everything, and those of these packages.
    Closure(BTreeSet<String>),
    /// A project's directory and its watch globs.
    Project { dir: String, watch: Vec<String> },
    /// Every markdown file.
    Markdown,
    /// Everything under a directory.
    Under(String),
    /// These files.
    Files(BTreeSet<String>),
}

/// One unit of work a pull request may skip: its name, what it reads, and
/// the settings of its command.
#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    pub id: String,
    pub reads: Vec<Reads>,
    salt: Vec<String>,
}

impl Lane {
    /// A lane that reads everything `parts` do, and `more`.
    pub fn union(id: &str, parts: &[&Lane], more: Vec<Reads>) -> Lane {
        let reads: BTreeSet<Reads> = parts
            .iter()
            .flat_map(|l| l.reads.iter().cloned())
            .chain(more)
            .collect();
        let salt: BTreeSet<String> = parts.iter().flat_map(|l| l.salt.iter().cloned()).collect();
        Lane {
            id: id.into(),
            reads: reads.into_iter().collect(),
            salt: salt.into_iter().collect(),
        }
    }
}

/// What the lanes of one run share.
pub struct Setup {
    /// Names the soothfast build that runs the lanes.
    pub tool: String,
    /// The current stable channel, which the jobs install by name.
    pub rust_stable: String,
    /// The runner labels as JSON.
    pub runner: String,
    pub shards: u64,
    pub pins: Pins,
}

/// The tree at head, as the lanes read it.
pub struct Inputs<'a> {
    files: &'a BTreeMap<String, String>,
    graph: Option<Graph>,
    test: TestConfig,
    ci_inputs: Vec<String>,
    salt: Vec<String>,
    rust_stable: String,
    pins: Pins,
}

impl<'a> Inputs<'a> {
    /// `files` is every file of the tree by blob id. `test` is the `[test]`
    /// table: its allow-lists are dropped, since a lane reads every file of
    /// the packages it builds. Without a tool name no lane has a fingerprint.
    pub fn new(
        files: &'a BTreeMap<String, String>,
        graph: Option<Graph>,
        test: TestConfig,
        ci_inputs: Vec<String>,
        setup: Setup,
    ) -> Option<Inputs<'a>> {
        if setup.tool.is_empty() {
            return None;
        }
        let env = binstore::ENV_NAMES
            .iter()
            .map(|n| n.to_string())
            .chain(["RUSTFLAGS".into(), "CARGO_ENCODED_RUSTFLAGS".into()])
            .chain(test.env.iter().cloned())
            .map(|n| format!("{n}={}", std::env::var(&n).unwrap_or_default()));
        let pins = Pins {
            runner: Some(setup.runner.clone()),
            ..setup.pins
        };
        let toml = files.get("soothfast.toml").map_or("", String::as_str);
        let salt = [
            format!("toml={toml}"),
            format!("cli={}", env!("CARGO_PKG_VERSION")),
            format!("tool={}", setup.tool),
            format!("runner={}", setup.runner),
            format!("shards={}", setup.shards),
            format!("java={}", pins.java.as_deref().unwrap_or_default()),
            format!("node={}", pins.node.as_deref().unwrap_or_default()),
        ]
        .into_iter()
        .chain(env)
        .collect();
        Some(Inputs {
            files,
            graph,
            test: TestConfig {
                packages: BTreeMap::new(),
                ..test
            },
            ci_inputs,
            salt,
            rust_stable: setup.rust_stable,
            pins,
        })
    }

    /// The lane that builds and runs `package`, or none when the workspace
    /// has no such package or the stable channel is unknown.
    pub fn rust(&self, id: &str, package: &str) -> Option<Lane> {
        let closure = self.graph.as_ref()?.closure(package);
        if closure.is_empty() || self.rust_stable.is_empty() {
            return None;
        }
        Some(Lane {
            id: id.into(),
            reads: vec![Reads::Closure(closure)],
            salt: vec![format!("stable={}", self.rust_stable)],
        })
    }

    /// The lane that benches a non-Rust project.
    pub fn project(&self, id: &str, project: &Project) -> Option<Lane> {
        let bench = project.bench.as_ref()?;
        let toolchain = toolchain_for(project.ecosystem, &self.pins);
        let settings = [
            ("cmd", bench.cmd.as_str()),
            ("out", &bench.out),
            ("from", &bench.from),
            ("walltime", project.walltime.as_deref().unwrap_or_default()),
            ("prefix", project.prefix.as_deref().unwrap_or_default()),
            ("toolchain", &toolchain),
        ];
        Some(Lane {
            id: id.into(),
            reads: vec![Reads::Project {
                dir: project.dir.clone(),
                watch: project.test.watch.clone(),
            }],
            salt: settings.map(|(k, v)| format!("{k}={v}")).to_vec(),
        })
    }

    /// Whether `lane` reads `file`.
    pub fn reads(&self, lane: &Lane, file: &str) -> bool {
        self.ci_inputs.iter().any(|g| glob::matches(g, file))
            || lane.reads.iter().any(|r| self.read_by(r, file))
    }

    fn read_by(&self, reads: &Reads, file: &str) -> bool {
        match reads {
            Reads::Closure(names) => {
                let pkgs = self.graph.as_ref().map_or(&[][..], Graph::packages);
                match classify(pkgs, &self.test, file) {
                    Scope::Workspace => true,
                    Scope::Package(name) => names.contains(name),
                    Scope::Ignored => false,
                }
            }
            Reads::Project { dir, watch } => {
                dir == "." || under(dir, file) || watch.iter().any(|g| glob::matches(g, file))
            }
            Reads::Markdown => file.ends_with(".md"),
            Reads::Under(dir) => under(dir, file),
            Reads::Files(paths) => paths.contains(file),
        }
    }

    /// The digest of `lane`'s settings and of every file it reads.
    pub fn fingerprint(&self, lane: &Lane) -> String {
        let files = self
            .files
            .iter()
            .filter(|(f, _)| self.reads(lane, f))
            .map(|(f, id)| format!("{f}\u{0}{id}"));
        let parts: Vec<String> = ["lane-fingerprint-v1".to_string(), lane.id.clone()]
            .into_iter()
            .chain(self.salt.iter().cloned())
            .chain(lane.salt.iter().cloned())
            .chain(files)
            .collect();
        hashed(&parts)
    }
}

fn under(dir: &str, file: &str) -> bool {
    file.strip_prefix(dir)
        .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::ecosystem::{Bench, Ecosystem, Tests};
    use crate::test::config::AllowList;
    use crate::test::fixture::TempRepo;
    use crate::test::project::Files;
    use serde_json::json;
    use std::path::Path;

    fn graph(root: &Path) -> Graph {
        let pkg = |name: &str, deps: &[&str]| {
            json!({
                "name": name,
                "manifest_path": format!("{}/{name}/Cargo.toml", root.display()),
                "dependencies": deps.iter().map(|d| json!({ "name": d })).collect::<Vec<_>>(),
            })
        };
        let meta = json!({ "packages": [
            pkg("app", &["lib"]),
            pkg("lib", &["fixtures"]),
            pkg("fixtures", &[]),
            pkg("sibling", &["lib"]),
        ]});
        Graph::from_metadata(&meta, root).unwrap()
    }

    fn repo(tag: &str) -> TempRepo {
        let repo = TempRepo::new(tag);
        for f in [
            "app/src/lib.rs",
            "lib/src/lib.rs",
            "fixtures/src/lib.rs",
            "sibling/src/lib.rs",
            "Cargo.lock",
            "soothfast.toml",
            "web/a.js",
            "docs/page.md",
            "assets/x.txt",
        ] {
            repo.write(f, "1");
        }
        repo.git(&["add", "."]);
        repo
    }

    fn setup() -> Setup {
        Setup {
            tool: "t1".into(),
            rust_stable: "1.90".into(),
            runner: "\"ubuntu-latest\"".into(),
            shards: 1,
            pins: Pins::default(),
        }
    }

    fn inputs<'a>(
        files: &'a BTreeMap<String, String>,
        root: &Path,
        test: TestConfig,
    ) -> Inputs<'a> {
        Inputs::new(files, Some(graph(root)), test, Vec::new(), setup()).unwrap()
    }

    fn print(repo: &TempRepo, test: TestConfig, id: &str, package: &str) -> String {
        let files = Files::read(&repo.root).unwrap();
        let inputs = inputs(files.ids(), &repo.root, test);
        inputs.fingerprint(&inputs.rust(id, package).unwrap())
    }

    fn app(repo: &TempRepo) -> String {
        print(repo, TestConfig::default(), "rust:app", "app")
    }

    #[test]
    fn a_package_and_everything_it_reaches_move_its_fingerprint() {
        let repo = repo("lane-fp-closure");
        let first = app(&repo);
        assert_eq!(first, app(&repo));
        for file in ["app/src/lib.rs", "lib/src/lib.rs", "fixtures/src/lib.rs"] {
            repo.write(file, "2");
            assert_ne!(first, app(&repo), "{file}");
            repo.write(file, "1");
        }
        assert_eq!(first, app(&repo));
    }

    #[test]
    fn a_sibling_package_leaves_it_alone_and_a_file_no_package_owns_does_not() {
        let repo = repo("lane-fp-sibling");
        let first = app(&repo);
        repo.write("sibling/src/lib.rs", "2");
        assert_eq!(first, app(&repo));
        repo.write("web/a.js", "2");
        assert_ne!(first, app(&repo));
    }

    #[test]
    fn a_build_wide_file_moves_every_rust_lane() {
        let repo = repo("lane-fp-wide");
        let first = app(&repo);
        let sibling = print(&repo, TestConfig::default(), "rust:sibling", "sibling");
        repo.write("Cargo.lock", "2");
        assert_ne!(first, app(&repo));
        let moved = print(&repo, TestConfig::default(), "rust:sibling", "sibling");
        assert_ne!(sibling, moved);
    }

    #[test]
    fn the_test_ignore_list_applies() {
        let repo = repo("lane-fp-ignore");
        let ignoring = || TestConfig {
            ignore: vec!["assets/**".into()],
            ..TestConfig::default()
        };
        let before = print(&repo, ignoring(), "rust:app", "app");
        let unignored = app(&repo);
        repo.write("assets/x.txt", "2");
        assert_eq!(before, print(&repo, ignoring(), "rust:app", "app"));
        assert_ne!(unignored, app(&repo));
    }

    #[test]
    fn a_package_allow_list_does_not_narrow_a_lane() {
        let repo = repo("lane-fp-allow");
        let narrow = || {
            let mut test = TestConfig::default();
            test.packages.insert(
                "lib".into(),
                AllowList {
                    header: String::new(),
                    files: vec!["src/**".into()],
                },
            );
            test
        };
        let before = print(&repo, narrow(), "rust:app", "app");
        repo.write("lib/data.json", "x");
        assert_ne!(before, print(&repo, narrow(), "rust:app", "app"));
    }

    #[test]
    fn each_shared_setting_moves_the_fingerprint() {
        let repo = repo("lane-fp-salt");
        let files = Files::read(&repo.root).unwrap();
        let print = |edit: &dyn Fn(&mut Setup, &mut Vec<String>)| {
            let mut setup = setup();
            let mut ci_inputs = Vec::new();
            edit(&mut setup, &mut ci_inputs);
            let test = TestConfig::default();
            let all = Inputs::new(files.ids(), Some(graph(&repo.root)), test, ci_inputs, setup);
            let inputs = all.unwrap();
            inputs.fingerprint(&inputs.rust("rust:app", "app").unwrap())
        };
        let first = print(&|_, _| {});
        assert_eq!(first, print(&|_, _| {}));
        assert_ne!(first, print(&|s, _| s.tool = "t2".into()));
        assert_ne!(first, print(&|s, _| s.rust_stable = "1.91".into()));
        assert_ne!(first, print(&|s, _| s.runner = "\"macos-latest\"".into()));
        assert_ne!(first, print(&|s, _| s.shards = 2));
        assert_ne!(first, print(&|s, _| s.pins.java = Some("17".into())));
        assert_ne!(first, print(&|s, _| s.pins.node = Some("22".into())));
        assert_ne!(first, print(&|_, c| c.push("sibling/**".into())));
    }

    #[test]
    fn the_root_soothfast_toml_moves_every_lane() {
        let repo = repo("lane-fp-toml");
        let first = app(&repo);
        repo.write("soothfast.toml", "[gate]\n");
        assert_ne!(first, app(&repo));
    }

    #[test]
    fn a_ci_input_glob_is_read_by_every_lane() {
        let repo = repo("lane-fp-ci-inputs");
        let files = Files::read(&repo.root).unwrap();
        let graph = Some(graph(&repo.root));
        let globs = vec!["web/**".into()];
        let all = Inputs::new(files.ids(), graph, TestConfig::default(), globs, setup());
        let inputs = all.unwrap();
        let lane = inputs.rust("rust:app", "app").unwrap();
        assert!(inputs.reads(&lane, "web/a.js"));
        assert!(!inputs.reads(&lane, "sibling/src/lib.rs"));
    }

    #[test]
    fn a_lane_without_a_tool_a_workspace_or_a_stable_channel_has_no_fingerprint() {
        let repo = repo("lane-fp-none");
        let files = Files::read(&repo.root).unwrap();
        let test = TestConfig::default;
        let mut untooled = setup();
        untooled.tool = String::new();
        assert!(Inputs::new(files.ids(), None, test(), vec![], untooled).is_none());
        let blind = Inputs::new(files.ids(), None, test(), vec![], setup()).unwrap();
        assert!(blind.rust("rust:app", "app").is_none());
        let mut unstable = setup();
        unstable.rust_stable = String::new();
        let graph = Some(graph(&repo.root));
        let inputs = Inputs::new(files.ids(), graph, test(), vec![], unstable).unwrap();
        assert!(inputs.rust("rust:app", "app").is_none());
        let inputs = inputs_of(&files, &repo);
        assert!(inputs.rust("rust:ghost", "ghost").is_none());
    }

    fn inputs_of<'a>(files: &'a Files, repo: &TempRepo) -> Inputs<'a> {
        inputs(files.ids(), &repo.root, TestConfig::default())
    }

    fn web() -> Project {
        Project {
            ecosystem: Ecosystem::Node,
            dir: "web".into(),
            bench: Some(Bench {
                cmd: "npm run bench".into(),
                out: "out.json".into(),
                from: "vitest".into(),
            }),
            walltime: None,
            prefix: None,
            test: Tests {
                watch: vec!["assets/**".into()],
                ..Tests::default()
            },
        }
    }

    #[test]
    fn a_project_lane_reads_its_directory_its_watch_globs_and_its_command() {
        let repo = repo("lane-fp-project");
        let print = |project: &Project| {
            let files = Files::read(&repo.root).unwrap();
            let inputs = inputs_of(&files, &repo);
            inputs.fingerprint(&inputs.project("import:node", project).unwrap())
        };
        let first = print(&web());
        repo.write("app/src/lib.rs", "2");
        assert_eq!(first, print(&web()));
        repo.write("web/a.js", "2");
        let moved = print(&web());
        assert_ne!(first, moved);
        repo.write("assets/x.txt", "2");
        assert_ne!(moved, print(&web()));
        let mut other = web();
        other.bench.as_mut().unwrap().cmd = "npm run other".into();
        assert_ne!(print(&web()), print(&other));
        other.bench = None;
        let files = Files::read(&repo.root).unwrap();
        assert!(
            inputs_of(&files, &repo)
                .project("import:node", &other)
                .is_none()
        );
    }

    #[test]
    fn the_docs_lane_reads_what_its_parts_do_and_every_markdown_file() {
        let repo = repo("lane-fp-docs");
        let files = Files::read(&repo.root).unwrap();
        let test = TestConfig {
            ignore: vec!["book/**".into()],
            ..TestConfig::default()
        };
        let inputs = inputs(files.ids(), &repo.root, test);
        let rust = inputs.rust("rust:sibling", "sibling").unwrap();
        let project = inputs.project("import:node", &web()).unwrap();
        let docs = Lane::union("docs", &[&rust, &project], vec![Reads::Markdown]);
        assert!(inputs.reads(&docs, "docs/page.md"));
        assert!(inputs.reads(&docs, "web/a.js"));
        assert!(inputs.reads(&docs, "sibling/src/lib.rs"));
        assert!(!inputs.reads(&docs, "app/src/lib.rs"));
        let site = Lane::union("check:site", &[&docs], vec![Reads::Under("book".into())]);
        assert!(inputs.reads(&site, "book/intro.txt"));
        assert!(!inputs.reads(&docs, "book/intro.txt"));
    }

    #[test]
    fn a_deleted_file_is_still_read_by_the_lane_that_read_it() {
        let repo = repo("lane-fp-gone");
        let files = Files::read(&repo.root).unwrap();
        let inputs = inputs_of(&files, &repo);
        let lane = inputs.rust("rust:app", "app").unwrap();
        assert!(inputs.reads(&lane, "lib/src/removed.rs"));
        assert!(!inputs.reads(&lane, "sibling/src/removed.rs"));
    }
}
