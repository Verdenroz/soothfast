//! The bench executable of a build: served from `binstore` when the tree
//! matches an entry, else built by cargo and kept for the next run.
//!
//! A stored executable is run directly, with the working directory and
//! `CARGO_MANIFEST_DIR` cargo gives a bench. Only a hit is run that way: a
//! target directory holding a fresh build is left to cargo.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use crate::binstore::{self, Request};
use crate::invoke::{self, Build, CommonArgs};
use crate::{rustdoc_inputs, store};

/// A stored executable and the directory cargo runs it from.
#[derive(Clone)]
pub struct Stored {
    pub exe: PathBuf,
    package_dir: PathBuf,
}

impl Stored {
    /// The command that runs the executable as `cargo bench` would, before
    /// any runner argument.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.exe);
        cmd.current_dir(&self.package_dir)
            .env("CARGO_MANIFEST_DIR", &self.package_dir)
            .arg("--bench");
        cmd
    }
}

/// What picks one bench binary within a process.
#[derive(Clone, PartialEq, Eq)]
struct Id {
    pkg: String,
    target: String,
    features: Option<String>,
    discover: bool,
    dir: Option<PathBuf>,
    target_dir: Option<PathBuf>,
}

impl Id {
    fn new(
        common: &CommonArgs,
        dir: Option<&Path>,
        target_dir: Option<&Path>,
        build: Build,
    ) -> Option<Id> {
        Some(Id {
            pkg: common.pkg.clone()?,
            target: common.target.as_deref().unwrap_or("soothfast").to_string(),
            features: common.features.clone(),
            discover: matches!(build, Build::Discover),
            dir: dir.map(Path::to_path_buf),
            target_dir: target_dir.map(Path::to_path_buf),
        })
    }
}

static SERVED: Mutex<Vec<(Id, Stored)>> = Mutex::new(Vec::new());

/// A build the store could hold, with the key its tree has before it starts.
struct Plan {
    id: Id,
    key: String,
    store: PathBuf,
    root: PathBuf,
    crate_dirs: Vec<PathBuf>,
    codegen_units: Option<String>,
    toolchain: Option<String>,
    rustc: String,
    target_dir: PathBuf,
    cargo_home: String,
    env: Vec<(String, String)>,
}

impl Plan {
    /// `None` when the build cannot be keyed: no package named, a tree or
    /// toolchain that cannot be read.
    fn new(
        common: &CommonArgs,
        dir: Option<&Path>,
        target_dir: Option<&Path>,
        build: Build,
    ) -> Option<Plan> {
        let id = Id::new(common, dir, target_dir, build)?;
        let root = match dir {
            Some(d) => d.to_path_buf(),
            None => invoke::workspace_root().ok()?,
        };
        let toolchain = invoke::bench_toolchain();
        let mut plan = Plan {
            crate_dirs: invoke::crate_dirs(&id.pkg, dir)?,
            codegen_units: common
                .codegen_units_env()
                .filter(|_| !id.discover)
                .map(str::to_string),
            id,
            key: String::new(),
            store: binstore::dir()?,
            rustc: store::rustc_version(toolchain.as_deref(), &root)?,
            toolchain,
            target_dir: match target_dir {
                Some(t) => t.to_path_buf(),
                None => invoke::target_dir(dir).ok()?,
            },
            cargo_home: std::env::var("CARGO_HOME")
                .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.cargo")))
                .ok()?,
            env: store::build_env(),
            root,
        };
        plan.key = plan.key_now()?;
        Some(plan)
    }

    fn key_now(&self) -> Option<String> {
        binstore::key(
            &self.root,
            &self.crate_dirs,
            &Request {
                pkg: &self.id.pkg,
                target: &self.id.target,
                profile: if self.id.discover { "dev" } else { "bench" },
                features: self.id.features.as_deref(),
                codegen_units: self.codegen_units.as_deref(),
                toolchain: self.toolchain.as_deref(),
                rustc: &self.rustc,
                target_dir: &self.target_dir,
                cargo_home: &self.cargo_home,
                env: &self.env,
            },
        )
    }

    /// The stored executable, remembered so that `served` finds it.
    fn serve(&self) -> Option<Stored> {
        let stored = Stored {
            exe: binstore::load(&self.store, &self.key, &self.root)?,
            package_dir: self.crate_dirs.first()?.clone(),
        };
        let mut served = SERVED.lock().unwrap_or_else(PoisonError::into_inner);
        served.retain(|(id, _)| *id != self.id);
        served.push((self.id.clone(), stored.clone()));
        Some(stored)
    }

    fn keep(&self, cargo_stdout: &[u8], exe: &Path, started: SystemTime) {
        if let Some(inputs) = rustdoc_inputs::collect_build(&self.root, exe, cargo_stdout) {
            binstore::store(
                &self.store,
                &self.key,
                self.key_now(),
                &self.root,
                &inputs,
                started,
                exe,
            );
        }
    }
}

/// The stored executable for this build, if the tree matches an entry.
pub fn lookup(
    common: &CommonArgs,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
    build: Build,
) -> Option<Stored> {
    Plan::new(common, dir, target_dir, build)?.serve()
}

/// The executable `lookup` found for this build in this process. It may be the
/// only build of it, so a run then needs no cargo.
pub fn served(
    common: &CommonArgs,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
    build: Build,
) -> Option<Stored> {
    let id = Id::new(common, dir, target_dir, build)?;
    let served = SERVED.lock().unwrap_or_else(PoisonError::into_inner);
    served
        .iter()
        .find(|(s, _)| *s == id)
        .map(|(_, v)| v.clone())
}

/// Path of the bench executable, from the store or else from
/// `cargo bench --no-run`, which then fills the store.
pub fn executable(
    common: &CommonArgs,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
    build: Build,
) -> Option<PathBuf> {
    let plan = Plan::new(common, dir, target_dir, build);
    let label = format!("bench executable {}", common.pkg.as_deref().unwrap_or(""));
    invoke::timed(&label, || {
        resolve(plan.as_ref(), || {
            let target = common.target.as_deref().unwrap_or("soothfast");
            let pkgs: Vec<&str> = common.pkg.as_deref().into_iter().collect();
            let mut cmd =
                invoke::bench_build_command(common, dir, target_dir, build, &pkgs).ok()?;
            cmd.args(["--no-run", "--message-format=json"]);
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::null());
            let out = cmd.output().ok().filter(|o| o.status.success())?;
            let exe = invoke::bench_artifact(&out.stdout, target)?;
            Some(Built {
                exe,
                cargo_stdout: out.stdout,
            })
        })
    })
}

/// What a cargo build left: the executable and the messages that name its inputs.
struct Built {
    exe: PathBuf,
    cargo_stdout: Vec<u8>,
}

/// The stored executable when `plan` has one, else what `build` makes, kept
/// for the next run. Cargo is not asked on a hit.
fn resolve(plan: Option<&Plan>, build: impl FnOnce() -> Option<Built>) -> Option<PathBuf> {
    if let Some(stored) = plan.and_then(Plan::serve) {
        return Some(stored.exe);
    }
    let started = SystemTime::now();
    let built = build()?;
    if let Some(plan) = plan {
        plan.keep(&built.cargo_stdout, &built.exe, started);
    }
    Some(built.exe)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use serde_json::json;

    use super::*;
    use crate::store::testing::Repo;

    fn plan(repo: &Repo, store: PathBuf) -> Plan {
        let mut plan = Plan {
            id: Id {
                pkg: "app".into(),
                target: "soothfast".into(),
                features: None,
                discover: false,
                dir: Some(repo.0.clone()),
                target_dir: None,
            },
            key: String::new(),
            store,
            root: repo.0.clone(),
            crate_dirs: repo.dirs().to_vec(),
            codegen_units: Some("1".into()),
            toolchain: None,
            rustc: "rustc 1.99.0".into(),
            target_dir: repo.0.join("target"),
            cargo_home: "/home/x/.cargo".into(),
            env: Vec::new(),
        };
        plan.key = plan.key_now().unwrap();
        plan
    }

    /// What `cargo bench --no-run` leaves for the app package: an executable,
    /// its dep-info and the message naming both.
    fn fake_build(repo: &Repo, bytes: &[u8]) -> Built {
        let deps = repo.0.join("target/release/deps");
        std::fs::create_dir_all(&deps).unwrap();
        let exe = deps.join("soothfast-1a2b");
        std::fs::write(&exe, bytes).unwrap();
        std::fs::write(
            deps.join("soothfast-1a2b.d"),
            format!(
                "{}: app/src/lib.rs docs/guide.md
",
                exe.display()
            ),
        )
        .unwrap();
        let message = json!({
            "reason": "compiler-artifact",
            "package_id": "path+file:///w/app#app@0.1.0",
            "manifest_path": repo.0.join("app/Cargo.toml"),
            "target": {"name": "soothfast", "kind": ["bench"]},
            "filenames": [exe],
            "executable": exe,
        });
        Built {
            exe,
            cargo_stdout: message.to_string().into_bytes(),
        }
    }

    fn resolve_counting(plan: &Plan, repo: &Repo, bytes: &[u8]) -> (PathBuf, u32) {
        let builds = Cell::new(0);
        let exe = resolve(Some(plan), || {
            builds.set(builds.get() + 1);
            Some(fake_build(repo, bytes))
        })
        .unwrap();
        (exe, builds.get())
    }

    #[test]
    fn a_hit_skips_cargo_and_returns_the_same_bytes() {
        let repo = Repo::new("exe-hit");
        repo.write(
            "docs/guide.md",
            "one
",
        );
        let store = repo.store_dir();
        let plan = plan(&repo, store.clone());
        let bytes = b"bench bytes".to_vec();

        let (built, builds) = resolve_counting(&plan, &repo, &bytes);
        assert_eq!(builds, 1);
        assert!(!built.starts_with(&store));

        let (hit, builds) = resolve_counting(&plan, &repo, b"never built");
        assert_eq!(builds, 0);
        assert!(hit.starts_with(&store));
        assert_eq!(std::fs::read(hit).unwrap(), bytes);
    }

    #[test]
    fn an_edit_to_a_file_the_build_read_is_a_miss() {
        let repo = Repo::new("exe-read");
        repo.write(
            "docs/guide.md",
            "one
",
        );
        let store = repo.store_dir();
        let plan = plan(&repo, store);
        resolve_counting(&plan, &repo, b"one");
        repo.write(
            "docs/guide.md",
            "two
",
        );
        let (exe, builds) = resolve_counting(&plan, &repo, b"two");
        assert_eq!(builds, 1);
        assert_eq!(std::fs::read(exe).unwrap(), b"two");
    }

    #[test]
    fn a_docs_only_edit_is_a_hit_and_a_source_edit_is_not() {
        let repo = Repo::new("exe-docs");
        repo.write(
            "docs/guide.md",
            "one
",
        );
        let store = repo.store_dir();
        resolve_counting(&plan(&repo, store.clone()), &repo, b"one");

        repo.write(
            "README.md",
            "edited
",
        );
        let (_, builds) = resolve_counting(&plan(&repo, store.clone()), &repo, b"x");
        assert_eq!(builds, 0);

        repo.write(
            "app/src/lib.rs",
            "pub fn app() -> u8 { 0 }
",
        );
        let (_, builds) = resolve_counting(&plan(&repo, store), &repo, b"y");
        assert_eq!(builds, 1);
    }

    #[test]
    fn a_build_without_readable_dep_info_is_not_stored() {
        let repo = Repo::new("exe-nodep");
        let store = repo.store_dir();
        let plan = plan(&repo, store);
        for _ in 0..2 {
            let builds = Cell::new(0);
            resolve(Some(&plan), || {
                builds.set(builds.get() + 1);
                let built = fake_build(&repo, b"one");
                std::fs::remove_file(built.exe.with_extension("d")).unwrap();
                Some(built)
            })
            .unwrap();
            assert_eq!(builds.get(), 1);
        }
    }
}
