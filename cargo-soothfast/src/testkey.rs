//! The pass cache: which test executables already passed, and under what
//! inputs.
//!
//! A key covers the executable's loaded code, every git-tracked file in its
//! package (sources, fixtures, goldens), files named by `[test] inputs`, the
//! files that select every package, the run arguments, the `[test] env`
//! variables and the rustc version.
//! Anything a test reads beyond those (files outside the package, the
//! network, the clock) is invisible here and can yield a stale pass.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::runcache::{self, hex_fnv};
use crate::test_config::TestConfig;
use crate::testbuild::TestBin;
use crate::testsel::{self, Package};
use crate::{glob, invoke};

/// Passes kept before the oldest are dropped.
const KEEP: usize = 2048;

/// Everything a key hashes besides the executable and its package.
pub struct Context<'a> {
    /// Digest of the files `[test] inputs` names.
    pub inputs: &'a str,
    /// Digest of the files that select every package.
    pub workspace: &'a str,
    pub run_args: &'a [String],
    /// `[test] env` variables with their current values.
    pub env: &'a [(String, String)],
    pub rustc: &'a str,
}

/// The current value of each named variable, empty when unset.
pub fn env_values(names: &[String]) -> Vec<(String, String)> {
    names
        .iter()
        .map(|n| (n.clone(), std::env::var(n).unwrap_or_default()))
        .collect()
}

/// The pass cache key for one executable, or `None` when its code cannot be
/// digested.
pub fn key(
    package: &str,
    target: &str,
    exe: &Path,
    package_inputs: &str,
    cx: &Context,
) -> Option<String> {
    let digest = crate::headrun::binary_digest(exe)?;
    Some(key_of(package, target, &digest, package_inputs, cx))
}

fn key_of(package: &str, target: &str, digest: &str, package_inputs: &str, cx: &Context) -> String {
    let env: Vec<String> = cx.env.iter().map(|(n, v)| format!("{n}={v}")).collect();
    let parts = [
        "soothfast-test-v2",
        package,
        target,
        digest,
        package_inputs,
        cx.inputs,
        cx.workspace,
        &cx.run_args.join("\u{1}"),
        &env.join("\u{1}"),
        cx.rustc,
    ];
    hex_fnv(parts.join("\u{2}").as_bytes())
}

/// Digest of a package's plain executables, which its integration tests may
/// spawn. `None` when any cannot be digested.
pub fn siblings_digest(siblings: &[&TestBin]) -> Option<String> {
    let digests = siblings
        .iter()
        .map(|b| Some((b.label(), crate::headrun::binary_digest(&b.exe)?)))
        .collect::<Option<Vec<_>>>()?;
    Some(fold_siblings(digests))
}

fn fold_siblings(mut digests: Vec<(String, String)>) -> String {
    digests.sort();
    let joined: Vec<String> = digests
        .iter()
        .map(|(l, d)| format!("{l}\u{0}{d}"))
        .collect();
    hex_fnv(joined.join("\n").as_bytes())
}

/// `rustc -vV`, which names the compiler by release and commit.
pub fn rustc_version() -> io::Result<String> {
    let out = std::process::Command::new("rustc").arg("-vV").output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Digest of the files under a package dir, tracked or not yet
/// committed.
pub fn package_inputs(root: &Path, dir: &str) -> io::Result<String> {
    let spec = if dir.is_empty() { "." } else { dir };
    digest(root, spec, |_| true)
}

/// Digest of the files any `[test] inputs` glob matches.
pub fn declared_inputs(root: &Path, globs: &[String]) -> io::Result<String> {
    if globs.is_empty() {
        return Ok(String::new());
    }
    digest(root, ".", |p| globs.iter().any(|g| glob::matches(g, p)))
}

/// Digest of the files whose change selects every package, so a run they
/// select cannot be served from a pass stored before they changed.
pub fn workspace_inputs(root: &Path, pkgs: &[Package], cfg: &TestConfig) -> io::Result<String> {
    digest(root, ".", |p| testsel::is_workspace_wide(pkgs, cfg, p))
}

/// Tracked files hash by git's blob id, modified and untracked ones by their
/// worktree content. Only files `keep` accepts are hashed.
fn digest(root: &Path, spec: &str, keep: impl Fn(&str) -> bool) -> io::Result<String> {
    let ls = |args: &[&str]| -> io::Result<String> {
        let mut full = vec!["-c", "core.quotepath=false", "ls-files"];
        full.extend(args);
        full.extend(["--", spec]);
        invoke::git_in(root, &full)
    };
    let mut entries: BTreeMap<String, Option<String>> = BTreeMap::new();
    for line in ls(&["-s"])?.lines() {
        if let Some((meta, path)) = line.split_once('\t') {
            entries.insert(path.to_string(), Some(meta.to_string()));
        }
    }
    for list in [ls(&["-m"])?, ls(&["--others", "--exclude-standard"])?] {
        entries.extend(list.lines().map(|path| (path.to_string(), None)));
    }
    let joined: Vec<String> = entries
        .into_iter()
        .filter(|(path, _)| keep(path))
        .map(|(path, meta)| {
            let hash = meta.unwrap_or_else(|| content_hash(&root.join(&path)));
            format!("{path}\u{0}{hash}")
        })
        .collect();
    Ok(hex_fnv(joined.join("\n").as_bytes()))
}

fn content_hash(path: &Path) -> String {
    std::fs::read(path).map_or_else(|_| "gone".into(), |b| hex_fnv(&b))
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

    #[cfg(test)]
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

    /// Store a pass under the key computed before the test ran, unless the
    /// key computed after it differs: a test that rewrites its own inputs
    /// would otherwise record a pass for inputs that no longer exist. A run
    /// with no key stores nothing. Whether the inputs were left alone.
    ///
    /// Best effort: a pass that cannot be written costs a re-run, nothing
    /// else.
    pub fn record_if_unchanged(
        &self,
        label: &str,
        before: Option<&str>,
        after: Option<&str>,
    ) -> bool {
        if before != after {
            return false;
        }
        if let Some(key) = before
            && std::fs::write(self.dir.join(key), label).is_ok()
        {
            runcache::prune_oldest(&self.dir, KEEP);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx<'a>(run_args: &'a [String], env: &'a [(String, String)]) -> Context<'a> {
        Context {
            inputs: "decl",
            workspace: "wide",
            run_args,
            env,
            rustc: "rustc 1",
        }
    }

    fn base() -> String {
        key_of("p", "t", "bin", "files", &cx(&[], &[]))
    }

    #[test]
    fn the_same_inputs_give_the_same_key() {
        assert_eq!(base(), base());
    }

    #[test]
    fn each_input_moves_the_key() {
        let filter = vec!["only_this".to_string()];
        let moved = [
            key_of("q", "t", "bin", "files", &cx(&[], &[])),
            key_of("p", "u", "bin", "files", &cx(&[], &[])),
            key_of("p", "t", "other", "files", &cx(&[], &[])),
            key_of("p", "t", "bin", "golden edited", &cx(&[], &[])),
            key_of("p", "t", "bin", "files", &cx(&filter, &[])),
            key_of(
                "p",
                "t",
                "bin",
                "files",
                &Context {
                    rustc: "rustc 2",
                    ..cx(&[], &[])
                },
            ),
            key_of(
                "p",
                "t",
                "bin",
                "files",
                &Context {
                    inputs: "decl2",
                    ..cx(&[], &[])
                },
            ),
            key_of(
                "p",
                "t",
                "bin",
                "files",
                &Context {
                    workspace: "wide2",
                    ..cx(&[], &[])
                },
            ),
        ];
        for k in &moved {
            assert_ne!(*k, base());
        }
    }

    #[test]
    fn a_sibling_binary_change_moves_the_key() {
        let digests = |d: &str| vec![("p cli (bin)".to_string(), d.to_string())];
        let key = |d: &str| key_of("p", "t", "bin", &fold_siblings(digests(d)), &cx(&[], &[]));
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
        let key = |v: &str| key_of("p", "t", "bin", "files", &cx(&[], &env(v)));
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
    fn a_workspace_wide_file_moves_the_workspace_inputs() {
        let root = std::env::temp_dir().join(format!("soothfast-wide-{}", std::process::id()));
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        std::fs::create_dir_all(root.join(".cargo")).unwrap();
        let git = |args: &[&str]| invoke::git_in(&root, args).unwrap();
        git(&["init", "-q"]);
        std::fs::write(root.join(".cargo/config.toml"), "[env]\nA = \"1\"\n").unwrap();
        std::fs::write(root.join("pkg/lib.rs"), "a").unwrap();
        git(&["add", "."]);
        let pkgs = vec![Package {
            name: "pkg".into(),
            dir: "pkg".into(),
            manifest_dir: root.join("pkg"),
            deps: Vec::new(),
        }];
        let cfg = TestConfig::default();
        let before = workspace_inputs(&root, &pkgs, &cfg).unwrap();
        std::fs::write(root.join("pkg/lib.rs"), "b").unwrap();
        let package_edit = workspace_inputs(&root, &pkgs, &cfg).unwrap();
        std::fs::write(root.join(".cargo/config.toml"), "[env]\nA = \"2\"\n").unwrap();
        let config_edit = workspace_inputs(&root, &pkgs, &cfg).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(before, package_edit);
        assert_ne!(before, config_edit);
    }

    #[test]
    fn a_pass_is_stored_only_when_the_test_left_its_inputs_alone() {
        let dir = std::env::temp_dir().join(format!("soothfast-settle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let passes = Passes::at(dir.clone());
        assert!(passes.record_if_unchanged("p t (lib)", Some("same"), Some("same")));
        assert!(passes.has("same"));
        assert!(!passes.record_if_unchanged("p t (lib)", Some("before"), Some("after")));
        assert!(!passes.has("before") && !passes.has("after"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_stored_pass_is_found_and_a_missing_one_is_not() {
        let dir = std::env::temp_dir().join(format!("soothfast-passes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let passes = Passes::at(dir.clone());
        assert!(!passes.has("abc"));
        passes.record_if_unchanged("p t (lib)", Some("abc"), Some("abc"));
        assert!(passes.has("abc"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn editing_a_rust_file_in_the_package_moves_its_inputs() {
        let root = std::env::temp_dir().join(format!("soothfast-inputs-{}", std::process::id()));
        std::fs::create_dir_all(root.join("pkg/golden")).unwrap();
        let git = |args: &[&str]| invoke::git_in(&root, args).unwrap();
        git(&["init", "-q"]);
        std::fs::write(root.join("pkg/golden/sample.rs"), "fn a() {}").unwrap();
        git(&["add", "."]);
        let committed = package_inputs(&root, "pkg").unwrap();
        std::fs::write(root.join("pkg/golden/sample.rs"), "fn b() {}").unwrap();
        let edited = package_inputs(&root, "pkg").unwrap();
        std::fs::write(root.join("pkg/golden/new.rs"), "").unwrap();
        assert_ne!(committed, edited);
        assert_ne!(edited, package_inputs(&root, "pkg").unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn content_hash_tracks_content_and_marks_a_missing_file() {
        let path = std::env::temp_dir().join(format!("soothfast-content-{}", std::process::id()));
        std::fs::write(&path, "a").unwrap();
        let first = content_hash(&path);
        std::fs::write(&path, "b").unwrap();
        assert_ne!(first, content_hash(&path));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(content_hash(&path), "gone");
    }
}
