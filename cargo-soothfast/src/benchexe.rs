//! The bench executable of a build: served from `binstore` when the tree
//! matches an entry, else built by cargo and kept for the next run.
//!
//! A stored executable is run directly, not through `cargo bench`. It gets the
//! working directory cargo gives a bench and the variables `CARGO_MANIFEST_DIR`
//! and `CARGO_PKG_NAME`; other `CARGO_*` variables, `LD_LIBRARY_PATH` and a
//! configured `runner` are not reproduced, so a tree with a runner is never
//! served.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use crate::binstore::{self, Request};
use crate::invoke::{self, Build, CommonArgs};
use crate::{rustdoc_inputs, store};

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

type Plans = Vec<(Id, Option<Arc<Plan>>)>;

/// The plan for this build, worked out once per process. `None` when the build
/// cannot be keyed or is run by a cargo runner, and so is left to cargo.
fn plan(
    common: &CommonArgs,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
    build: Build,
) -> Option<Arc<Plan>> {
    static PLANS: Mutex<Plans> = Mutex::new(Vec::new());
    let id = Id::new(common, dir, target_dir, build)?;
    let known = PLANS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, plan)) = known.iter().find(|(i, _)| *i == id) {
        return plan.clone();
    }
    drop(known);
    let plan = Plan::new(common, id.clone(), dir).map(Arc::new);
    PLANS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((id, plan.clone()));
    plan
}

impl Plan {
    fn new(common: &CommonArgs, id: Id, dir: Option<&Path>) -> Option<Plan> {
        let root = match dir {
            Some(d) => d.to_path_buf(),
            None => invoke::workspace_root().ok()?,
        };
        let cargo_home = std::env::var("CARGO_HOME")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.cargo")))
            .ok()?;
        if runner_configured(&root, &cargo_home) {
            return None;
        }
        let toolchain = invoke::bench_toolchain();
        let mut plan = Plan {
            crate_dirs: invoke::crate_dirs(&id.pkg, dir)?,
            codegen_units: common
                .codegen_units_env()
                .filter(|_| !id.discover)
                .map(str::to_string),
            target_dir: match &id.target_dir {
                Some(t) => t.clone(),
                None => invoke::target_dir(dir).ok()?,
            },
            rustc: store::rustc_version(toolchain.as_deref(), &root)?,
            id,
            key: String::new(),
            store: binstore::dir()?,
            toolchain,
            cargo_home,
            env: store::build_env(&binstore::ENV_NAMES),
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

    /// The stored executable, if its file is there and its inputs hold.
    fn load(&self) -> Option<PathBuf> {
        binstore::load(&self.store, &self.key, &self.root)
    }

    /// What one stored executable stands for: a package's bench target built
    /// under one profile, in one checkout and target directory.
    fn group(&self) -> String {
        let profile = if self.id.discover { "dev" } else { "bench" };
        format!(
            "{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{profile}",
            self.root.display(),
            self.target_dir.display(),
            self.id.pkg,
            self.id.target,
            self.id.features.as_deref().unwrap_or(""),
        )
    }

    fn keep(&self, cargo_stdout: &[u8], exe: &Path, started: SystemTime) {
        let pkg = &self.id.pkg;
        let inputs = match rustdoc_inputs::collect_build(&self.root, exe, cargo_stdout) {
            Ok(inputs) => inputs,
            Err(why) => {
                eprintln!("soothfast: {pkg} not stored: dep-info unreadable: {why}");
                return;
            }
        };
        let key_now = self.key_now();
        if key_now.as_deref() != Some(&self.key) {
            eprintln!("soothfast: {pkg} not stored: the tree changed during the build");
            return;
        }
        let kept = binstore::store(
            &self.store,
            &self.key,
            key_now,
            &self.root,
            &inputs,
            started,
            exe,
            &self.group(),
        );
        if kept.is_none() && self.load().is_none() {
            eprintln!(
                "soothfast: {pkg} not stored: an input changed during the build or the entry could not be written"
            );
        }
    }

    /// `exe` run as `cargo bench` would, before any runner argument.
    fn command(&self, exe: &Path) -> Option<Command> {
        let package_dir = self.crate_dirs.first()?;
        let mut cmd = Command::new(exe);
        cmd.current_dir(package_dir)
            .env("CARGO_MANIFEST_DIR", package_dir)
            .env("CARGO_PKG_NAME", &self.id.pkg)
            .arg("--bench");
        Some(cmd)
    }
}

/// Whether cargo would hand the executable to a configured runner. Any mention
/// of one counts, since a runner changes what is measured.
fn runner_configured(root: &Path, cargo_home: &str) -> bool {
    let in_env = std::env::vars_os().any(|(name, _)| {
        name.to_str()
            .is_some_and(|n| n.starts_with("CARGO_TARGET_") && n.ends_with("_RUNNER"))
    });
    let home = Path::new(cargo_home);
    in_env
        || [
            root.join(".cargo/config.toml"),
            root.join(".cargo/config"),
            home.join("config.toml"),
            home.join("config"),
        ]
        .iter()
        .any(|f| std::fs::read_to_string(f).is_ok_and(|text| text.contains("runner")))
}

/// The command that runs this build's bench executable, when the store can
/// supply it. With `fill`, a miss is built and kept first, so one cargo
/// invocation both builds and serves. `None` leaves the run to `cargo bench`.
pub fn run_command(
    common: &CommonArgs,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
    build: Build,
    fill: bool,
) -> Option<Command> {
    let plan = plan(common, dir, target_dir, build)?;
    let exe = match plan.load() {
        Some(exe) => exe,
        None if fill => resolve(Some(&plan), || cargo_build(common, dir, target_dir, build))?,
        None => return None,
    };
    plan.command(&exe)
}

/// Whether the store holds this build's executable.
pub fn is_stored(
    common: &CommonArgs,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
    build: Build,
) -> bool {
    plan(common, dir, target_dir, build).is_some_and(|plan| plan.load().is_some())
}

/// Path of the bench executable, from the store or else from
/// `cargo bench --no-run`, which then fills the store.
pub fn executable(
    common: &CommonArgs,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
    build: Build,
) -> Option<PathBuf> {
    let plan = plan(common, dir, target_dir, build);
    if plan.is_none() {
        eprintln!(
            "soothfast: {} not stored: the build cannot be keyed or a cargo runner is configured",
            common.pkg.as_deref().unwrap_or("")
        );
    }
    let label = format!("bench executable {}", common.pkg.as_deref().unwrap_or(""));
    invoke::timed(&label, || {
        resolve(plan.as_deref(), || {
            cargo_build(common, dir, target_dir, build)
        })
    })
}

fn cargo_build(
    common: &CommonArgs,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
    build: Build,
) -> Option<Built> {
    let target = common.target.as_deref().unwrap_or("soothfast");
    let pkgs: Vec<&str> = common.pkg.as_deref().into_iter().collect();
    let mut cmd = invoke::bench_build_command(common, dir, target_dir, build, &pkgs).ok()?;
    cmd.args(["--no-run", "--message-format=json"]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    let exe = invoke::bench_artifact(&out.stdout, target)?;
    Some(Built {
        exe,
        cargo_stdout: out.stdout,
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
    if let Some(exe) = plan.and_then(Plan::load) {
        return Some(exe);
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
                dir: None,
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
        fake_build_in(repo, "release", bytes)
    }

    fn fake_build_in(repo: &Repo, profile_dir: &str, bytes: &[u8]) -> Built {
        let deps = repo.0.join("target").join(profile_dir).join("deps");
        std::fs::create_dir_all(&deps).unwrap();
        let exe = deps.join("soothfast-1a2b");
        std::fs::write(&exe, bytes).unwrap();
        std::fs::write(
            deps.join("soothfast-1a2b.d"),
            format!("{}: app/src/lib.rs docs/guide.md\n", exe.display()),
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

    /// The same build when the package also has a build script, whose artifact
    /// names the linked `build-script-build` beside the hashed file.
    fn fake_build_with_script(repo: &Repo, bytes: &[u8]) -> Built {
        let mut built = fake_build(repo, bytes);
        let dir = repo.0.join("target/release/build/app-9f");
        std::fs::create_dir_all(&dir).unwrap();
        let hashed = dir.join("build_script_build-9f");
        std::fs::write(&hashed, "script").unwrap();
        std::fs::write(
            dir.join("build_script_build-9f.d"),
            format!("{}: app/build.rs\n", hashed.display()),
        )
        .unwrap();
        let linked = dir.join("build-script-build");
        let _ = std::fs::remove_file(&linked);
        std::fs::hard_link(&hashed, &linked).unwrap();
        let message = json!({
            "reason": "compiler-artifact",
            "package_id": "path+file:///w/app#app@0.1.0",
            "manifest_path": repo.0.join("app/Cargo.toml"),
            "target": {"name": "build-script-build", "kind": ["custom-build"],
                       "src_path": repo.0.join("app/build.rs")},
            "filenames": [linked],
        });
        built
            .cargo_stdout
            .extend(format!("\n{message}").into_bytes());
        built
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
        repo.write("docs/guide.md", "one\n");
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
        repo.write("docs/guide.md", "one\n");
        let store = repo.store_dir();
        let plan = plan(&repo, store);
        resolve_counting(&plan, &repo, b"one");
        repo.write("docs/guide.md", "two\n");
        let (exe, builds) = resolve_counting(&plan, &repo, b"two");
        assert_eq!(builds, 1);
        assert_eq!(std::fs::read(exe).unwrap(), b"two");
    }

    #[test]
    fn a_docs_only_edit_is_a_hit_and_a_source_edit_is_not() {
        let repo = Repo::new("exe-docs");
        repo.write("docs/guide.md", "one\n");
        let store = repo.store_dir();
        resolve_counting(&plan(&repo, store.clone()), &repo, b"one");

        repo.write("README.md", "edited\n");
        let (_, builds) = resolve_counting(&plan(&repo, store.clone()), &repo, b"x");
        assert_eq!(builds, 0);

        repo.write("app/src/lib.rs", "pub fn app() -> u8 { 0 }\n");
        let (_, builds) = resolve_counting(&plan(&repo, store), &repo, b"y");
        assert_eq!(builds, 1);
    }

    #[test]
    fn a_package_with_a_build_script_is_stored() {
        let repo = Repo::new("exe-script");
        repo.write("app/build.rs", "fn main() {}\n");
        let store = repo.store_dir();
        let plan = plan(&repo, store);
        for expected in [1, 0] {
            let builds = Cell::new(0);
            resolve(Some(&plan), || {
                builds.set(builds.get() + 1);
                Some(fake_build_with_script(&repo, b"one"))
            })
            .unwrap();
            assert_eq!(builds.get(), expected);
        }
        repo.write("app/build.rs", "fn main() { println!(); }\n");
        let (_, builds) = resolve_counting(&self::plan(&repo, plan.store.clone()), &repo, b"y");
        assert_eq!(builds, 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_stored_entry_that_lost_its_mode_still_runs() {
        use std::os::unix::fs::PermissionsExt;

        let repo = Repo::new("exe-mode");
        let plan = plan(&repo, repo.store_dir());
        let script = b"#!/bin/sh\nexit 0\n";
        let (stored, _) = resolve_counting(&plan, &repo, script);
        assert!(!stored.starts_with(&plan.store));
        let (entry, builds) = resolve_counting(&plan, &repo, b"never built");
        assert_eq!(builds, 0);
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o644)).unwrap();

        let hit = plan.load().unwrap();
        assert_eq!(hit, entry);
        assert_ne!(
            std::fs::metadata(&hit).unwrap().permissions().mode() & 0o111,
            0
        );
        assert!(Command::new(&hit).status().unwrap().success());
    }

    #[test]
    fn a_dev_profile_routes_build_is_stored_apart_from_the_bench_one() {
        let repo = Repo::new("exe-routes");
        let store = repo.store_dir();
        let bench = plan(&repo, store.clone());
        let mut routes = plan(&repo, store);
        routes.id.discover = true;
        routes.codegen_units = None;
        routes.key = routes.key_now().unwrap();
        assert_ne!(routes.key, bench.key);
        assert_ne!(routes.group(), bench.group());
        for expected in [1, 0] {
            let builds = Cell::new(0);
            let exe = resolve(Some(&routes), || {
                builds.set(builds.get() + 1);
                Some(fake_build_in(&repo, "debug", b"routes"))
            })
            .unwrap();
            assert_eq!(builds.get(), expected);
            assert_eq!(std::fs::read(exe).unwrap(), b"routes");
        }
        assert!(routes.load().is_some());
        assert!(bench.load().is_none());
    }

    #[test]
    fn a_scratch_directory_made_during_the_build_does_not_stop_the_store() {
        let repo = Repo::new("exe-scratch");
        let store = repo.store_dir();
        repo.write(".gitignore", "/target\n");
        let mut plan = plan(&repo, store);
        plan.crate_dirs = vec![repo.0.clone()];
        plan.key = plan.key_now().unwrap();
        for expected in [1, 0] {
            let builds = Cell::new(0);
            resolve(Some(&plan), || {
                builds.set(builds.get() + 1);
                repo.write(".soothfast-action/Cargo.toml", "[package]\n");
                repo.write(".soothfast-branch/runs/a.json", "{}");
                Some(fake_build(&repo, b"one"))
            })
            .unwrap();
            assert_eq!(builds.get(), expected);
        }
        assert!(plan.load().is_some());
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

    #[test]
    fn the_head_and_a_worktree_build_keep_an_entry_each() {
        let head = Repo::new("exe-head");
        let base = Repo::new("exe-base");
        let store = head.store_dir();
        let (head_plan, base_plan) = (plan(&head, store.clone()), plan(&base, store));
        let (head_exe, _) = resolve_counting(&head_plan, &head, b"head");
        let (base_exe, _) = resolve_counting(&base_plan, &base, b"base");
        assert_ne!(head_plan.group(), base_plan.group());
        for (p, r) in [(&head_plan, &head), (&base_plan, &base)] {
            let (_, builds) = resolve_counting(p, r, b"never built");
            assert_eq!(builds, 0);
        }
        assert!(head_exe.exists() && base_exe.exists());
    }

    #[test]
    fn a_stored_run_gets_the_working_directory_and_package_variables() {
        let repo = Repo::new("exe-env");
        let plan = plan(&repo, repo.store_dir());
        let cmd = plan.command(Path::new("/x/bench")).unwrap();
        assert_eq!(cmd.get_current_dir(), Some(repo.0.join("app").as_path()));
        let envs: Vec<_> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_str().unwrap(), v.and_then(|v| v.to_str())))
            .collect();
        let manifest_dir = repo.0.join("app");
        assert_eq!(
            envs,
            [
                ("CARGO_MANIFEST_DIR", manifest_dir.to_str()),
                ("CARGO_PKG_NAME", Some("app")),
            ]
        );
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["--bench"]);
    }

    #[test]
    fn a_configured_runner_is_noticed() {
        let repo = Repo::new("exe-runner");
        assert!(!runner_configured(&repo.0, "/nowhere"));
        repo.write(
            ".cargo/config.toml",
            "[target.x86_64-unknown-linux-gnu]\nrunner = \"qemu\"\n",
        );
        assert!(runner_configured(&repo.0, "/nowhere"));
    }
}
