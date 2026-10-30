//! Rustdoc JSON kept on disk, keyed by everything that produces it.
//!
//! Building it means compiling the package and every dependency under
//! nightly, which dwarfs every step after it, and none of it is shared with
//! another job or run. Lookup is in two levels, as a compiler cache does it.
//! A cheap key names the toolchain, flags, lockfile and the files git lists in
//! the package directories. It selects a manifest of the other files and
//! variables the last build read (from rustdoc and cargo dep-info). The entry
//! is named for the key and a digest of those inputs, so any of them changing
//! is a miss, in any checkout, including the merge-base worktree.
//!
//! A tree the key cannot cover (a package directory outside the repository,
//! no git) has no key and is always rebuilt. What a build reads without
//! saying so, and configuration outside the repository such as
//! `~/.cargo/config.toml`, is not covered.

use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use serde_json::{Value, json};
use soothfast_registry::fnv1a;

use crate::invoke::{self, Visibility};
use crate::runcache;

/// Bytes kept before the least recently used entries are dropped.
pub(crate) const MAX_BYTES: u64 = 1 << 30;

/// Variable families that change what rustc and rustdoc compile.
const ENV_PREFIXES: [&str; 4] = [
    "CARGO_ENCODED_",
    "CARGO_BUILD_",
    "CARGO_PROFILE_",
    "CARGO_TARGET_",
];

/// Members of those families that only say where or how fast to build.
const ENV_IGNORED: [&str; 3] = [
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "CARGO_BUILD_JOBS",
];

/// Files at the workspace root that configure the build without being part
/// of any package.
const ROOT_FILES: [&str; 6] = [
    "Cargo.toml",
    "Cargo.lock",
    ".cargo/config.toml",
    ".cargo/config",
    "rust-toolchain",
    "rust-toolchain.toml",
];

/// One extraction, as far as the key is concerned.
pub struct Request<'a> {
    pub pkg: &'a str,
    pub features: Option<&'a str>,
    pub visibility: Visibility,
    /// `rustc -vV` of the toolchain that documents it.
    pub rustc: &'a str,
    pub env: &'a [(String, String)],
}

/// The files and variables a build read beyond the package directories.
/// Paths are relative to the workspace root when they lie under it.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Inputs {
    pub files: BTreeSet<String>,
    pub env: BTreeSet<String>,
}

impl Inputs {
    fn to_json(&self) -> Value {
        json!({ "files": self.files, "env": self.env })
    }

    fn from_json(v: &Value) -> Option<Inputs> {
        let strings = |k: &str| -> Option<BTreeSet<String>> {
            v[k].as_array()?
                .iter()
                .map(|s| s.as_str().map(str::to_string))
                .collect()
        };
        Some(Inputs {
            files: strings("files")?,
            env: strings("env")?,
        })
    }

    /// A hash of what the inputs hold now, or `None` when one cannot be read.
    fn digest(&self, root: &Path) -> Option<String> {
        let mut acc = String::new();
        for file in &self.files {
            acc.push_str(&format!("{file}\u{1}{}\n", hash_path(&root.join(file))?));
        }
        for name in &self.env {
            let value = std::env::var(name).ok();
            acc.push_str(&format!("{name}={value:?}\n"));
        }
        Some(format!("{:016x}", fnv1a(acc.as_bytes())))
    }

    /// Whether any input was modified after `started`, so the build may have
    /// read it before or after an edit.
    fn changed_since(&self, root: &Path, started: SystemTime) -> bool {
        self.files
            .iter()
            .any(|f| modified_after(&root.join(f), started))
    }
}

/// A token for the content at `path`. A directory hashes everything under it
/// (bar `.git`), so a nested repository or submodule changes the token when
/// its files do. `None` when something cannot be read.
fn hash_path(path: &Path) -> Option<String> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some("absent".into()),
        Err(_) => return None,
    };
    if !meta.is_dir() {
        return Some(format!("file:{:016x}", fnv1a(&std::fs::read(path).ok()?)));
    }
    let mut names = std::fs::read_dir(path)
        .ok()?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    names.sort();
    let mut acc = String::new();
    for name in names.iter().filter(|n| *n != ".git") {
        acc.push_str(&format!(
            "{}\u{1}{}\n",
            name.to_string_lossy(),
            hash_path(&path.join(name))?
        ));
    }
    Some(format!("dir:{:016x}", fnv1a(acc.as_bytes())))
}

fn modified_after(path: &Path, started: SystemTime) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.modified().is_ok_and(|t| t > started) {
        return true;
    }
    meta.is_dir()
        && std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name() != ".git")
            .any(|e| modified_after(&e.path(), started))
}

/// `rustc +toolchain -vV`, which names the release, commit and host.
pub fn rustc_version(toolchain: &str) -> Option<String> {
    static SEEN: Mutex<Vec<(String, Option<String>)>> = Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, version)) = seen.iter().find(|(t, _)| t == toolchain) {
        return version.clone();
    }
    let version = Command::new("rustc")
        .args([&format!("+{toolchain}"), "-vV"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    seen.push((toolchain.to_string(), version.clone()));
    version
}

/// The set variables that change what rustc and rustdoc compile.
pub fn build_env() -> Vec<(String, String)> {
    select_env(
        std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))),
    )
}

fn select_env(vars: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut picked: Vec<_> = vars
        .filter(|(name, _)| {
            matches!(name.as_str(), "RUSTFLAGS" | "RUSTDOCFLAGS")
                || (ENV_PREFIXES.iter().any(|p| name.starts_with(p))
                    && !ENV_IGNORED.contains(&name.as_str()))
        })
        .collect();
    picked.sort();
    picked
}

/// The key for `req` built from the tree at `root`, whose package sources
/// live in `crate_dirs`. `None` when part of the input cannot be read.
pub fn key(root: &Path, crate_dirs: &[PathBuf], req: &Request) -> Option<String> {
    if crate_dirs.is_empty() {
        return None;
    }
    let rels = crate_dirs
        .iter()
        .map(|d| d.strip_prefix(root).ok().map(Path::to_path_buf))
        .collect::<Option<Vec<_>>>()?;

    let mut files: BTreeSet<PathBuf> = tracked_files(root, &rels)?
        .into_iter()
        .filter(|f| !f.starts_with(".soothfast") && !f.starts_with(".github"))
        .collect();
    files.extend(ROOT_FILES.iter().map(PathBuf::from));

    let mut input = format!(
        "{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\n",
        req.pkg,
        req.features.unwrap_or(""),
        req.visibility == Visibility::Private,
        invoke::RUSTDOC_FORMAT_VERSION,
        req.rustc,
    );
    for (name, value) in req.env {
        input.push_str(&format!("{name}={value}\n"));
    }
    for file in files {
        let token = hash_path(&root.join(&file))?;
        input.push_str(&format!("{}\u{1}{token}\n", file.display()));
    }
    Some(format!("{:016x}", fnv1a(input.as_bytes())))
}

/// Files under `rels` that git tracks or would add, so uncommitted edits are
/// part of the key and ignored build output is not.
fn tracked_files(root: &Path, rels: &[PathBuf]) -> Option<Vec<PathBuf>> {
    let mut cmd = Command::new("git");
    cmd.args([
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
    ]);
    for rel in rels {
        cmd.arg(if rel.as_os_str().is_empty() {
            Path::new(".")
        } else {
            rel
        });
    }
    let out = cmd
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(
        out.stdout
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| PathBuf::from(String::from_utf8_lossy(p).into_owned()))
            .collect(),
    )
}

/// The document stored under `key` whose inputs still hold what they did.
pub fn load(key: &str, root: &Path) -> Option<Value> {
    load_from(&runcache::state_dir("rustdoc")?, key, root)
}

/// Keep the JSON rustdoc wrote at `json` for a build that started at
/// `started`, unless the tree was edited meanwhile: `key` must still be the
/// key the tree computes (`key_now`), and no input may be newer than the build.
pub fn store(
    key: &str,
    key_now: Option<String>,
    root: &Path,
    inputs: &Inputs,
    started: SystemTime,
    json: &Path,
) {
    if key_now.as_deref() != Some(key) {
        return;
    }
    if let Some(dir) = runcache::state_dir("rustdoc") {
        store_in(&dir, key, root, inputs, started, json);
    }
}

fn entry_path(dir: &Path, key: &str, digest: &str) -> PathBuf {
    dir.join(format!("{key}-{digest}.json"))
}

fn load_from(dir: &Path, key: &str, root: &Path) -> Option<Value> {
    let manifest = std::fs::read_to_string(dir.join(format!("{key}.manifest"))).ok()?;
    let inputs = Inputs::from_json(&serde_json::from_str(&manifest).ok()?)?;
    let path = entry_path(dir, key, &inputs.digest(root)?);
    let doc = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    if let Ok(file) = File::options().write(true).open(&path) {
        let _ = file.set_modified(SystemTime::now());
    }
    Some(doc)
}

fn store_in(dir: &Path, key: &str, root: &Path, inputs: &Inputs, started: SystemTime, json: &Path) {
    if inputs.changed_since(root, started) {
        return;
    }
    let Some(digest) = inputs.digest(root) else {
        return;
    };
    let manifest = dir.join(format!("{key}.manifest"));
    let stored = write_atomic(&manifest, inputs.to_json().to_string().as_bytes())
        && copy_atomic(json, &entry_path(dir, key, &digest));
    if stored {
        prune(dir, MAX_BYTES);
    }
}

/// A name no other process or call shares, so concurrent writers never
/// interleave in one file.
fn temp_for(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{n}.tmp", std::process::id()));
    path.with_file_name(name)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> bool {
    let tmp = temp_for(path);
    std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, path).is_ok()
}

fn copy_atomic(from: &Path, to: &Path) -> bool {
    let tmp = temp_for(to);
    std::fs::copy(from, &tmp).is_ok() && std::fs::rename(&tmp, to).is_ok()
}

/// Remove the least recently used entries of `dir` beyond `max_bytes`,
/// always keeping the newest. Files being written (`*.tmp`) are left alone.
pub fn prune(dir: &Path, max_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter(|e| e.path().extension().is_none_or(|x| x != "tmp"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((meta.modified().ok()?, meta.len(), e.path()))
        })
        .collect();
    files.sort_by_key(|(t, _, _)| std::cmp::Reverse(*t));
    let mut kept = 0;
    for (i, (_, len, path)) in files.into_iter().enumerate() {
        kept += len;
        if i > 0 && kept > max_bytes {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    struct Repo(PathBuf);

    impl Repo {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "soothfast-rustdocstore-{tag}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let repo = Repo(root);
            repo.git(&["init", "-q"]);
            for (path, text) in [
                ("Cargo.toml", "[workspace]\n"),
                ("Cargo.lock", "# lock\n"),
                ("README.md", "readme\n"),
                ("app/src/lib.rs", "pub fn app() {}\n"),
                ("dep/src/lib.rs", "pub fn dep() {}\n"),
                ("other/src/lib.rs", "pub fn other() {}\n"),
            ] {
                repo.write(path, text);
            }
            repo
        }

        fn git(&self, args: &[&str]) {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&self.0)
                .status()
                .unwrap()
                .success();
            assert!(ok);
        }

        fn write(&self, path: &str, text: &str) {
            let full = self.0.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }

        fn key_with(&self, req: &Request) -> String {
            let dirs = [self.0.join("app"), self.0.join("dep")];
            key(&self.0, &dirs, req).unwrap()
        }

        fn key(&self) -> String {
            self.key_with(&request())
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn request() -> Request<'static> {
        Request {
            pkg: "app",
            features: None,
            visibility: Visibility::Private,
            rustc: "rustc 1.99.0-nightly",
            env: &[],
        }
    }

    fn inputs(files: &[&str]) -> Inputs {
        Inputs {
            files: files.iter().map(|f| f.to_string()).collect(),
            env: BTreeSet::new(),
        }
    }

    #[test]
    fn the_key_is_stable_and_follows_the_package_sources() {
        let repo = Repo::new("sources");
        let before = repo.key();
        assert_eq!(before, repo.key());
        repo.write("app/src/lib.rs", "pub fn app() -> u8 { 0 }\n");
        let after_app = repo.key();
        assert_ne!(before, after_app);
        repo.write("dep/src/lib.rs", "pub fn dep() -> u8 { 0 }\n");
        assert_ne!(after_app, repo.key());
    }

    #[test]
    fn the_key_follows_the_lockfile_and_workspace_manifest() {
        let repo = Repo::new("lock");
        let before = repo.key();
        repo.write("Cargo.lock", "# lock 2\n");
        let relocked = repo.key();
        assert_ne!(before, relocked);
        repo.write("Cargo.toml", "[workspace]\nresolver = \"3\"\n");
        assert_ne!(relocked, repo.key());
    }

    #[test]
    fn the_key_ignores_files_outside_the_packages() {
        let repo = Repo::new("outside");
        let before = repo.key();
        repo.write("README.md", "edited\n");
        repo.write("other/src/lib.rs", "pub fn other() -> u8 { 0 }\n");
        assert_eq!(before, repo.key());

        let dirs = [repo.0.clone()];
        let root_package = key(&repo.0, &dirs, &request()).unwrap();
        repo.write(".soothfast/rustdoc/entry.json", "{}");
        repo.write(".github/workflows/ci.yml", "on: push\n");
        assert_eq!(root_package, key(&repo.0, &dirs, &request()).unwrap());
        repo.write("README.md", "edited again\n");
        assert_ne!(root_package, key(&repo.0, &dirs, &request()).unwrap());
    }

    #[test]
    fn the_key_follows_features_visibility_toolchain_and_flags() {
        let repo = Repo::new("request");
        let base = repo.key();
        let flags = [("RUSTFLAGS".to_string(), "--cfg x".to_string())];
        let variants = [
            Request {
                features: Some("a"),
                ..request()
            },
            Request {
                visibility: Visibility::Public,
                ..request()
            },
            Request {
                rustc: "rustc 1.99.1-nightly",
                ..request()
            },
            Request {
                env: &flags,
                ..request()
            },
            Request {
                pkg: "dep",
                ..request()
            },
        ];
        let keys: BTreeSet<_> = variants.iter().map(|r| repo.key_with(r)).collect();
        assert_eq!(keys.len(), variants.len());
        assert!(!keys.contains(&base));
    }

    #[test]
    fn every_cargo_flag_family_is_part_of_the_environment() {
        let vars = [
            ("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS", "-Cx"),
            ("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTDOCFLAGS", "-Cy"),
            ("CARGO_PROFILE_DEV_OPT_LEVEL", "3"),
            ("CARGO_BUILD_RUSTFLAGS", "-Cz"),
            ("CARGO_ENCODED_RUSTFLAGS", "a\u{1f}b"),
            ("RUSTFLAGS", "-Cw"),
            ("RUSTDOCFLAGS", "-Cv"),
            ("CARGO_TARGET_DIR", "/t"),
            ("CARGO_BUILD_JOBS", "4"),
            ("HOME", "/home/x"),
        ];
        let picked = select_env(vars.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let names: Vec<_> = picked.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            [
                "CARGO_BUILD_RUSTFLAGS",
                "CARGO_ENCODED_RUSTFLAGS",
                "CARGO_PROFILE_DEV_OPT_LEVEL",
                "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTDOCFLAGS",
                "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
                "RUSTDOCFLAGS",
                "RUSTFLAGS",
            ]
        );
    }

    #[test]
    fn a_nested_repository_changes_the_key_when_its_files_do() {
        let repo = Repo::new("nested");
        repo.write("app/vendor/lib/src/lib.rs", "pub fn v() {}\n");
        let nested = repo.0.join("app/vendor/lib");
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&nested)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let before = repo.key();
        repo.write("app/vendor/lib/src/lib.rs", "pub fn v() -> u8 { 0 }\n");
        assert_ne!(before, repo.key());
    }

    #[test]
    fn source_outside_the_repository_has_no_key() {
        let repo = Repo::new("outside-dir");
        let elsewhere = std::env::temp_dir();
        assert_eq!(key(&repo.0, &[elsewhere], &request()), None);
        assert_eq!(key(&repo.0, &[], &request()), None);
    }

    #[test]
    fn a_file_outside_the_package_read_by_the_build_changes_the_entry() {
        let repo = Repo::new("manifest");
        repo.write("docs/guide.md", "one\n");
        let json = repo.0.join("app.json");
        std::fs::write(&json, r#"{"format_version":61}"#).unwrap();
        let store = repo.0.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let read = inputs(&["docs/guide.md"]);
        let future = SystemTime::now() + Duration::from_secs(3600);
        store_in(&store, "k", &repo.0, &read, future, &json);
        assert!(load_from(&store, "k", &repo.0).is_some());
        repo.write("docs/guide.md", "two\n");
        assert_eq!(load_from(&store, "k", &repo.0), None);
        repo.write("docs/guide.md", "one\n");
        assert!(load_from(&store, "k", &repo.0).is_some());
    }

    #[test]
    fn a_directory_input_follows_what_is_under_it() {
        let repo = Repo::new("dirinput");
        repo.write("gen/a.txt", "a\n");
        let read = inputs(&["gen"]);
        let before = read.digest(&repo.0).unwrap();
        repo.write("gen/sub/b.txt", "b\n");
        assert_ne!(before, read.digest(&repo.0).unwrap());
    }

    #[test]
    fn a_missing_input_differs_from_an_empty_one() {
        let repo = Repo::new("absent");
        let read = inputs(&["gen.rs"]);
        let absent = read.digest(&repo.0).unwrap();
        repo.write("gen.rs", "");
        assert_ne!(absent, read.digest(&repo.0).unwrap());
    }

    #[test]
    fn an_edit_during_the_build_is_not_stored() {
        let repo = Repo::new("race");
        repo.write("docs/guide.md", "one\n");
        let json = repo.0.join("app.json");
        std::fs::write(&json, "{}").unwrap();
        let store = repo.0.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let read = inputs(&["docs/guide.md"]);
        let before_edit = SystemTime::now() - Duration::from_secs(3600);
        store_in(&store, "k", &repo.0, &read, before_edit, &json);
        assert_eq!(load_from(&store, "k", &repo.0), None);

        let wrong_key = Some("other".to_string());
        super::store("k", wrong_key, &repo.0, &read, before_edit, &json);
        assert_eq!(load_from(&store, "k", &repo.0), None);
    }

    #[test]
    fn concurrent_stores_leave_one_readable_entry() {
        let repo = Repo::new("concurrent");
        repo.write("docs/guide.md", "one\n");
        let json = repo.0.join("app.json");
        let text = format!(r#"{{"index":"{}"}}"#, "x".repeat(1 << 20));
        std::fs::write(&json, &text).unwrap();
        let store = repo.0.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let read = inputs(&["docs/guide.md"]);
        let future = SystemTime::now() + Duration::from_secs(3600);
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| store_in(&store, "k", &repo.0, &read, future, &json));
            }
        });
        let doc = load_from(&store, "k", &repo.0).unwrap();
        assert_eq!(doc["index"].as_str().map(str::len), Some(1 << 20));
        let temps = std::fs::read_dir(&store)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"))
            .count();
        assert_eq!(temps, 0);
    }

    #[test]
    fn a_stored_document_reads_back_identical() {
        let repo = Repo::new("roundtrip");
        let json = repo.0.join("app.json");
        let text = r#"{"format_version":61,"index":{"0":{"name":"app"}}}"#;
        std::fs::write(&json, text).unwrap();
        let store = repo.0.join("store");
        std::fs::create_dir_all(&store).unwrap();
        assert_eq!(load_from(&store, "abc", &repo.0), None);
        let future = SystemTime::now() + Duration::from_secs(3600);
        store_in(&store, "abc", &repo.0, &inputs(&[]), future, &json);
        let expected: Value = serde_json::from_str(text).unwrap();
        assert_eq!(load_from(&store, "abc", &repo.0), Some(expected));
    }

    #[test]
    fn pruning_drops_the_least_recently_used_beyond_the_cap_and_spares_temp_files() {
        let dir = std::env::temp_dir().join(format!("soothfast-rdprune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        for (name, age) in [("old", 30), ("mid", 20), ("new", 10), ("w.1.0.tmp", 40)] {
            let path = dir.join(name);
            std::fs::write(&path, [0u8; 10]).unwrap();
            let file = File::options().write(true).open(&path).unwrap();
            file.set_modified(now - Duration::from_secs(age)).unwrap();
        }
        prune(&dir, 20);
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["mid", "new", "w.1.0.tmp"]);
        prune(&dir, 0);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
