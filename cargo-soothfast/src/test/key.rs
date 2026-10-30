//! The pass cache: which test executables already passed, and under what
//! inputs.
//!
//! A key covers the executable's loaded code, the package's plain
//! executables, every git-tracked file in its package (sources, fixtures,
//! goldens), the files that select every package (`[test] inputs`
//! included), the run arguments, the `[test] env` variables and the rustc
//! version.
//! Anything a test reads beyond those (files outside the package, the
//! network, the clock) is invisible here and can yield a stale pass.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use super::cargo::TestBin;
use super::config::TestConfig;
use super::select::{self, Package, Scope};
use crate::runcache::{self, hashed};
use crate::{buildstamp, headrun, invoke};

/// Passes kept before the oldest are dropped.
const KEEP: usize = 2048;

/// Computes pass cache keys against the files as they are right now. The
/// workspace digest is taken once, a package's digests on first use.
pub struct Keyer {
    root: PathBuf,
    pkgs: Vec<Package>,
    cfg: TestConfig,
    run_args: Vec<String>,
    env: Vec<(String, String)>,
    rustc: String,
    siblings: Vec<TestBin>,
    workspace: String,
    files: BTreeMap<String, String>,
    sibling_sets: BTreeMap<String, Option<String>>,
    /// Executable digests by path, taken once per round: the run step is the
    /// build's own invocation, so nothing relinks an executable within it.
    exes: BTreeMap<PathBuf, Option<String>>,
}

impl Keyer {
    /// A keyer for one round, `siblings` being the plain executables the
    /// build produced.
    pub fn new(
        root: PathBuf,
        pkgs: Vec<Package>,
        cfg: TestConfig,
        run_args: Vec<String>,
        siblings: Vec<TestBin>,
    ) -> Result<Keyer, String> {
        let workspace = workspace_inputs(&root, &pkgs, &cfg).map_err(|e| e.to_string())?;
        Ok(Keyer {
            rustc: buildstamp::rustc_vv(None).map_err(|e| e.to_string())?,
            env: env_values(&cfg.env),
            root,
            pkgs,
            cfg,
            run_args,
            siblings,
            workspace,
            files: BTreeMap::new(),
            sibling_sets: BTreeMap::new(),
            exes: BTreeMap::new(),
        })
    }

    /// The pass cache key for `bin`, or `None` when its code or a sibling's
    /// cannot be digested.
    pub fn key(&mut self, bin: &TestBin) -> Result<Option<String>, String> {
        let Some(pkg) = self.pkgs.iter().find(|p| p.name == bin.package) else {
            return Ok(None);
        };
        if !self.files.contains_key(&bin.package) {
            let files = package_inputs(&self.root, &self.pkgs, &self.cfg, pkg)
                .map_err(|e| e.to_string())?;
            self.files.insert(bin.package.clone(), files);
        }
        if !self.sibling_sets.contains_key(&bin.package) {
            let siblings = self.siblings_digest(&bin.package);
            self.sibling_sets.insert(bin.package.clone(), siblings);
        }
        let Some((siblings, digest)) = self.sibling_sets[&bin.package]
            .clone()
            .zip(self.exe_digest(&bin.exe))
        else {
            return Ok(None);
        };
        Ok(Some(key_of(KeyParts {
            package: &bin.package,
            target: &bin.target,
            digest: &digest,
            files: &self.files[&bin.package],
            siblings: &siblings,
            workspace: &self.workspace,
            run_args: &self.run_args,
            env: &self.env,
            rustc: &self.rustc,
        })))
    }

    /// `bin`'s key with the workspace and its package's files digested
    /// afresh, to tell whether running it changed its own inputs.
    pub fn rekey(&mut self, bin: &TestBin) -> Result<Option<String>, String> {
        self.workspace =
            workspace_inputs(&self.root, &self.pkgs, &self.cfg).map_err(|e| e.to_string())?;
        self.files.remove(&bin.package);
        self.key(bin)
    }

    fn exe_digest(&mut self, exe: &Path) -> Option<String> {
        self.exes
            .entry(exe.to_path_buf())
            .or_insert_with(|| headrun::binary_digest(exe))
            .clone()
    }

    /// Digest of a package's plain executables, which its integration tests
    /// may spawn. `None` when any cannot be digested.
    fn siblings_digest(&mut self, package: &str) -> Option<String> {
        let siblings: Vec<(String, PathBuf)> = self
            .siblings
            .iter()
            .filter(|s| s.package == package)
            .map(|s| (s.label(), s.exe.clone()))
            .collect();
        let digests = siblings
            .into_iter()
            .map(|(label, exe)| Some((label, self.exe_digest(&exe)?)))
            .collect::<Option<Vec<_>>>()?;
        Some(fold_siblings(digests))
    }
}

/// The current value of each named variable, empty when unset.
fn env_values(names: &[String]) -> Vec<(String, String)> {
    names
        .iter()
        .map(|n| (n.clone(), std::env::var(n).unwrap_or_default()))
        .collect()
}

#[derive(Clone, Copy)]
struct KeyParts<'a> {
    package: &'a str,
    target: &'a str,
    digest: &'a str,
    files: &'a str,
    siblings: &'a str,
    workspace: &'a str,
    run_args: &'a [String],
    env: &'a [(String, String)],
    rustc: &'a str,
}

fn key_of(p: KeyParts) -> String {
    let env: Vec<String> = p.env.iter().map(|(n, v)| format!("{n}={v}")).collect();
    hashed(&[
        "soothfast-test-v3".into(),
        p.package.into(),
        p.target.into(),
        p.digest.into(),
        p.files.into(),
        p.siblings.into(),
        p.workspace.into(),
        p.run_args.join("\u{2}"),
        env.join("\u{2}"),
        p.rustc.into(),
    ])
}

fn fold_siblings(mut digests: Vec<(String, String)>) -> String {
    digests.sort();
    let joined: Vec<String> = digests
        .iter()
        .map(|(l, d)| format!("{l}\u{0}{d}"))
        .collect();
    hashed(&joined)
}

/// Digest of the files under a package dir, tracked or not yet
/// committed.
fn package_inputs(
    root: &Path,
    pkgs: &[Package],
    cfg: &TestConfig,
    pkg: &Package,
) -> io::Result<String> {
    let spec = if pkg.dir.is_empty() { "." } else { &pkg.dir };
    digest(root, spec, |p| {
        select::classify(pkgs, cfg, p) == Scope::Package(&pkg.name)
    })
}

/// Digest of the files whose change selects every package, so a run they
/// select cannot be served from a pass stored before they changed.
fn workspace_inputs(root: &Path, pkgs: &[Package], cfg: &TestConfig) -> io::Result<String> {
    digest(root, ".", |p| {
        select::classify(pkgs, cfg, p) == Scope::Workspace
    })
}

/// Every file hashes to the blob id git would store for its content, read
/// from the index when clean and computed from the worktree when modified or
/// untracked, so a change digests the same before and after it is committed.
/// Only files `keep` accepts are hashed.
fn digest(root: &Path, spec: &str, keep: impl Fn(&str) -> bool) -> io::Result<String> {
    let ls = |args: &[&str]| -> io::Result<String> {
        let mut full = vec!["-c", "core.quotepath=false", "ls-files"];
        full.extend(args);
        full.extend(["--", spec]);
        invoke::git_in(root, &full)
    };
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    for line in ls(&["-s"])?.lines() {
        if let Some((meta, path)) = line.split_once('\t') {
            let id = meta.split_whitespace().nth(1).unwrap_or_default();
            ids.insert(path.to_string(), id.to_string());
        }
    }
    let mut dirty: Vec<String> = Vec::new();
    for list in [ls(&["-m"])?, ls(&["--others", "--exclude-standard"])?] {
        dirty.extend(list.lines().filter(|p| keep(p)).map(String::from));
    }
    let (present, gone): (Vec<String>, Vec<String>) =
        dirty.into_iter().partition(|p| root.join(p).is_file());
    ids.extend(gone.into_iter().map(|p| (p, "gone".to_string())));
    ids.extend(blob_ids(root, &present)?);
    let joined: Vec<String> = ids
        .into_iter()
        .filter(|(path, _)| keep(path))
        .map(|(path, id)| format!("{path}\u{0}{id}"))
        .collect();
    Ok(hashed(&joined))
}

/// The blob ids git computes for worktree files, a batch per call.
fn blob_ids(root: &Path, paths: &[String]) -> io::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for chunk in paths.chunks(256) {
        let mut args = vec!["hash-object", "--"];
        args.extend(chunk.iter().map(String::as_str));
        let listing = invoke::git_in(root, &args)?;
        out.extend(chunk.iter().cloned().zip(listing.lines().map(String::from)));
    }
    Ok(out)
}

/// Keys of executables that passed, one small file each.
pub struct Passes {
    dir: PathBuf,
}

impl Passes {
    /// `.soothfast/tests`, created on demand.
    pub fn open() -> Option<Passes> {
        Some(Passes {
            dir: runcache::state_dir("tests")?,
        })
    }

    pub(crate) fn at(dir: PathBuf) -> Passes {
        Passes { dir }
    }

    /// Whether `key` has a stored pass. A hit is rewritten so pruning drops
    /// passes nobody asks for first.
    pub fn has(&self, key: &str) -> bool {
        let path = self.dir.join(key);
        match std::fs::read(&path) {
            Ok(label) => {
                let _ = std::fs::write(&path, label);
                true
            }
            Err(_) => false,
        }
    }

    /// Store a pass for `label` under `key`.
    ///
    /// Best effort: a pass that cannot be written costs a re-run, nothing
    /// else.
    pub fn record(&self, label: &str, key: &str) {
        if std::fs::write(self.dir.join(key), label).is_ok() {
            runcache::prune_oldest(&self.dir, KEEP);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::fixture::TempRepo;

    const BASE: KeyParts = KeyParts {
        package: "p",
        target: "t",
        digest: "bin",
        files: "files",
        siblings: "spawned",
        workspace: "wide",
        run_args: &[],
        env: &[],
        rustc: "rustc 1",
    };

    fn base() -> String {
        key_of(BASE)
    }

    #[test]
    fn the_same_inputs_give_the_same_key() {
        assert_eq!(base(), base());
    }

    #[test]
    fn each_input_moves_the_key() {
        let filter = vec!["only_this".to_string()];
        let moved = [
            KeyParts {
                package: "q",
                ..BASE
            },
            KeyParts {
                target: "u",
                ..BASE
            },
            KeyParts {
                digest: "other",
                ..BASE
            },
            KeyParts {
                files: "golden edited",
                ..BASE
            },
            KeyParts {
                run_args: &filter,
                ..BASE
            },
            KeyParts {
                rustc: "rustc 2",
                ..BASE
            },
            KeyParts {
                workspace: "wide2",
                ..BASE
            },
        ];
        for k in moved {
            assert_ne!(key_of(k), base());
        }
    }

    #[test]
    fn a_sibling_binary_change_moves_the_key() {
        let digests = |d: &str| vec![("p cli (bin)".to_string(), d.to_string())];
        let key = |d: &str| {
            key_of(KeyParts {
                siblings: &fold_siblings(digests(d)),
                ..BASE
            })
        };
        assert_eq!(key("one"), key("one"));
        assert_ne!(key("one"), key("two"));
    }

    #[test]
    fn sibling_order_does_not_matter() {
        let a = ("a".to_string(), "1".to_string());
        let b = ("b".to_string(), "2".to_string());
        assert_eq!(
            fold_siblings(vec![a.clone(), b.clone()]),
            fold_siblings(vec![b, a])
        );
    }

    #[test]
    fn a_listed_env_value_moves_the_key() {
        let env = |v: &str| vec![("DATABASE_URL".to_string(), v.to_string())];
        let key = |v: &str| {
            key_of(KeyParts {
                env: &env(v),
                ..BASE
            })
        };
        assert_eq!(key("a"), key("a"));
        assert_ne!(key("a"), key("b"));
        assert_ne!(key("a"), base());
    }

    #[test]
    fn env_values_default_to_empty_when_unset() {
        let got = env_values(&["SOOTHFAST_SURELY_UNSET_VAR".to_string()]);
        assert_eq!(
            got,
            [("SOOTHFAST_SURELY_UNSET_VAR".to_string(), String::new())]
        );
    }

    #[test]
    fn a_root_package_hashes_neither_nested_members_nor_ignored_files() {
        let repo = TempRepo::new("rootpkg");
        repo.write("src/lib.rs", "a");
        repo.write("server/src/main.rs", "a");
        repo.write("docs/index.md", "a");
        repo.write("tests/fixtures/q.json", "a");
        repo.git(&["add", "."]);
        let pkgs = [
            package("root", "", &repo.root),
            package("server", "server", &repo.root),
        ];
        let cfg = TestConfig {
            ignore: vec!["docs/**".into(), "tests/fixtures/q.json".into()],
            inputs: vec!["tests/fixtures/q.json".into()],
            ..TestConfig::default()
        };
        let root_key = || package_inputs(&repo.root, &pkgs, &cfg, &pkgs[0]).unwrap();
        let before = root_key();
        repo.write("server/src/main.rs", "b");
        repo.write("docs/index.md", "b");
        assert_eq!(before, root_key());
        repo.write("tests/fixtures/q.json", "b");
        assert_eq!(before, root_key());
        repo.write("src/lib.rs", "b");
        assert_ne!(before, root_key());
    }

    #[test]
    fn a_workspace_wide_file_moves_the_workspace_inputs() {
        let repo = TempRepo::new("wide");
        repo.write(".cargo/config.toml", "[env]\nA = \"1\"\n");
        repo.write("pkg/lib.rs", "a");
        repo.git(&["add", "."]);
        let pkgs = vec![Package {
            name: "pkg".into(),
            dir: "pkg".into(),
            manifest_dir: repo.root.join("pkg"),
            deps: Vec::new(),
        }];
        let cfg = TestConfig::default();
        let before = workspace_inputs(&repo.root, &pkgs, &cfg).unwrap();
        repo.write("pkg/lib.rs", "b");
        let package_edit = workspace_inputs(&repo.root, &pkgs, &cfg).unwrap();
        repo.write(".cargo/config.toml", "[env]\nA = \"2\"\n");
        let config_edit = workspace_inputs(&repo.root, &pkgs, &cfg).unwrap();
        assert_eq!(before, package_edit);
        assert_ne!(before, config_edit);
    }

    #[test]
    fn a_declared_input_inside_a_package_moves_the_workspace_inputs() {
        let repo = TempRepo::new("declared");
        repo.write("pkg/shared/f.json", "1");
        repo.git(&["add", "."]);
        let pkgs = vec![Package {
            name: "pkg".into(),
            dir: "pkg".into(),
            manifest_dir: repo.root.join("pkg"),
            deps: Vec::new(),
        }];
        let cfg = TestConfig {
            inputs: vec!["pkg/shared/**".into()],
            ..TestConfig::default()
        };
        let before = workspace_inputs(&repo.root, &pkgs, &cfg).unwrap();
        repo.write("pkg/shared/f.json", "2");
        assert_ne!(before, workspace_inputs(&repo.root, &pkgs, &cfg).unwrap());
    }

    #[test]
    fn a_stored_pass_is_found_and_a_missing_one_is_not() {
        let dir = std::env::temp_dir().join(format!("soothfast-passes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let passes = Passes::at(dir.clone());
        assert!(!passes.has("abc"));
        passes.record("p t (lib)", "abc");
        assert!(passes.has("abc"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn package(name: &str, dir: &str, root: &Path) -> Package {
        Package {
            name: name.into(),
            dir: dir.into(),
            manifest_dir: root.join(dir),
            deps: Vec::new(),
        }
    }

    fn inputs_of(root: &Path, dir: &str) -> String {
        let pkgs = [package("pkg", dir, root)];
        package_inputs(root, &pkgs, &TestConfig::default(), &pkgs[0]).unwrap()
    }

    #[test]
    fn editing_a_rust_file_in_the_package_moves_its_inputs() {
        let repo = TempRepo::new("inputs");
        repo.write("pkg/golden/sample.rs", "fn a() {}");
        repo.git(&["add", "."]);
        let committed = inputs_of(&repo.root, "pkg");
        repo.write("pkg/golden/sample.rs", "fn b() {}");
        let edited = inputs_of(&repo.root, "pkg");
        repo.write("pkg/golden/new.rs", "");
        assert_ne!(committed, edited);
        assert_ne!(edited, inputs_of(&repo.root, "pkg"));
    }

    #[test]
    fn a_file_digests_the_same_dirty_and_committed_and_differently_once_deleted() {
        let repo = TempRepo::new("same");
        repo.write("pkg/data.json", "{}");
        let untracked = inputs_of(&repo.root, "pkg");
        repo.git(&["add", "."]);
        let staged = inputs_of(&repo.root, "pkg");
        repo.commit();
        let committed = inputs_of(&repo.root, "pkg");
        std::fs::remove_file(repo.root.join("pkg/data.json")).unwrap();
        let deleted = inputs_of(&repo.root, "pkg");
        assert_eq!(untracked, staged);
        assert_eq!(staged, committed);
        assert_ne!(committed, deleted);
    }

    #[cfg(unix)]
    struct Round {
        repo: TempRepo,
        keyer: Keyer,
        tests: [TestBin; 2],
    }

    #[cfg(unix)]
    fn bin(repo: &TempRepo, target: &str, kind: &str) -> TestBin {
        let exe = repo.root.join("exes").join(target);
        std::os::unix::fs::symlink(std::env::current_exe().unwrap(), &exe).unwrap();
        TestBin {
            package: "pkg".into(),
            target: target.into(),
            kinds: vec![kind.into()],
            doctest: false,
            exe,
        }
    }

    #[cfg(unix)]
    /// Package `pkg` with two test binaries and a `tool` sibling, every
    /// executable a symlink to this test binary.
    fn round(name: &str) -> Round {
        let repo = TempRepo::new(name);
        repo.write(".gitignore", "exes/\n");
        repo.write("pkg/src/lib.rs", "");
        std::fs::create_dir_all(repo.root.join("exes")).unwrap();
        let tests = [bin(&repo, "a", "test"), bin(&repo, "b", "test")];
        let sibling = bin(&repo, "tool", "bin");
        let keyer = Keyer::new(
            repo.root.clone(),
            vec![package("pkg", "pkg", &repo.root)],
            TestConfig::default(),
            Vec::new(),
            vec![sibling],
        )
        .unwrap();
        Round { repo, keyer, tests }
    }

    #[cfg(unix)]
    #[test]
    fn rekeying_reuses_the_rounds_executable_digests() {
        let Round {
            repo,
            mut keyer,
            tests,
        } = round("rekey-memo");
        let keys: Vec<_> = tests.iter().map(|t| keyer.key(t).unwrap()).collect();
        assert!(keys.iter().all(Option::is_some));
        for exe in ["a", "b", "tool"] {
            std::fs::remove_file(repo.root.join("exes").join(exe)).unwrap();
        }
        for (test, key) in tests.iter().zip(&keys) {
            assert_eq!(&keyer.rekey(test).unwrap(), key);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_package_file_one_test_writes_moves_the_next_tests_rekey() {
        let Round {
            repo,
            mut keyer,
            tests,
        } = round("rekey-files");
        let [a, b] = &tests;
        let (ka, kb) = (keyer.key(a).unwrap(), keyer.key(b).unwrap());
        assert_eq!(keyer.rekey(a).unwrap(), ka);
        repo.write("pkg/golden/out.json", "{}");
        assert_ne!(keyer.rekey(b).unwrap(), kb);
    }

    #[test]
    fn a_root_allow_list_keeps_unlisted_files_out_of_both_digests() {
        let repo = TempRepo::new("allow-key");
        repo.write("src/lib.rs", "pub fn a() {}");
        repo.git(&["add", "."]);
        let pkgs = [package("pkg", "", &repo.root)];
        let mut cfg = TestConfig::default();
        let list = crate::test::config::AllowList {
            header: "test.packages.pkg".into(),
            files: vec!["src/**".into()],
        };
        cfg.packages.insert("pkg".into(), list);
        let digests = || {
            (
                package_inputs(&repo.root, &pkgs, &cfg, &pkgs[0]).unwrap(),
                workspace_inputs(&repo.root, &pkgs, &cfg).unwrap(),
            )
        };
        let before = digests();
        repo.write("docker-compose.yml", "services: {}");
        repo.write("monitoring/dash.json", "{}");
        assert_eq!(digests(), before);
        repo.write("src/more.rs", "");
        let after = digests();
        assert_ne!(after.0, before.0);
        assert_eq!(after.1, before.1);
    }
}
