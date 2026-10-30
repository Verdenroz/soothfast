//! Rustdoc JSON kept on disk, keyed by everything that produces it.
//!
//! Building it means compiling the package and every dependency under
//! nightly, which dwarfs every step after it, and none of it is shared with
//! another job or run. An entry is named for the toolchain, flags, lockfile
//! and source it was built from, so the same inputs read back the same
//! document in any checkout, including the merge-base worktree.
//!
//! A tree the key cannot cover (source outside the repository, no git) has no
//! key and is always rebuilt. The key does not cover what a build script
//! reads beyond the variables in [`ENV`], or configuration outside the
//! repository such as `~/.cargo/config.toml`.

use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use serde_json::Value;
use soothfast_registry::fnv1a;

use crate::invoke::{self, Visibility};
use crate::runcache;

/// Bytes kept before the least recently used entries are dropped.
pub(crate) const MAX_BYTES: u64 = 1 << 30;

/// Variables that change what rustc and rustdoc compile.
const ENV: [&str; 7] = [
    "RUSTFLAGS",
    "RUSTDOCFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_ENCODED_RUSTDOCFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_BUILD_RUSTDOCFLAGS",
    "CARGO_BUILD_TARGET",
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

/// `rustc +toolchain -vV`, which names the release, commit and host.
pub fn rustc_version(toolchain: &str) -> Option<String> {
    let out = Command::new("rustc")
        .args([&format!("+{toolchain}"), "-vV"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The variables of [`ENV`] that are set.
pub fn build_env() -> Vec<(String, String)> {
    ENV.iter()
        .filter_map(|name| Some((name.to_string(), std::env::var(name).ok()?)))
        .collect()
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

    let mut files = BTreeSet::new();
    for rel in &rels {
        for file in tracked_files(root, rel)? {
            let nested = rels
                .iter()
                .any(|other| other != rel && other.starts_with(rel) && file.starts_with(other));
            if !nested && !file.starts_with(".soothfast") {
                files.insert(file);
            }
        }
    }
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
        let digest = std::fs::read(root.join(&file)).map_or(0, |b| fnv1a(&b));
        input.push_str(&format!("{}\u{1}{digest:016x}\n", file.display()));
    }
    Some(format!("{:016x}", fnv1a(input.as_bytes())))
}

/// Files under `rel` that git tracks or would add, so uncommitted edits are
/// part of the key and ignored build output is not.
fn tracked_files(root: &Path, rel: &Path) -> Option<Vec<PathBuf>> {
    let pathspec = if rel.as_os_str().is_empty() {
        Path::new(".")
    } else {
        rel
    };
    let out = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .arg("--")
        .arg(pathspec)
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

/// The document stored under `key`.
pub fn load(key: &str) -> Option<Value> {
    load_from(&runcache::state_dir("rustdoc")?, key)
}

/// Keep the JSON rustdoc wrote at `json` under `key`.
pub fn store(key: &str, json: &Path) {
    if let Some(dir) = runcache::state_dir("rustdoc") {
        store_in(&dir, key, json);
    }
}

fn load_from(dir: &Path, key: &str) -> Option<Value> {
    let path = dir.join(format!("{key}.json"));
    let doc = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    if let Ok(file) = File::options().write(true).open(&path) {
        let _ = file.set_modified(SystemTime::now());
    }
    Some(doc)
}

fn store_in(dir: &Path, key: &str, json: &Path) {
    let path = dir.join(format!("{key}.json"));
    let tmp = path.with_extension("tmp");
    if std::fs::copy(json, &tmp).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
        prune(dir, MAX_BYTES);
    }
}

/// Remove the least recently used entries of `dir` beyond `max_bytes`,
/// always keeping the newest.
pub fn prune(dir: &Path, max_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
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
    fn a_nested_package_is_hashed_once_under_its_own_directory() {
        let repo = Repo::new("nested");
        let dirs = [repo.0.clone(), repo.0.join("dep")];
        let req = request();
        let before = key(&repo.0, &dirs, &req).unwrap();
        repo.write("other/src/lib.rs", "pub fn other() -> u8 { 0 }\n");
        assert_ne!(before, key(&repo.0, &dirs, &req).unwrap());
        let edited = key(&repo.0, &dirs, &req).unwrap();
        repo.write(".soothfast/rustdoc/entry.json", "{}");
        assert_eq!(edited, key(&repo.0, &dirs, &req).unwrap());
        let moved = [repo.0.join("dep"), repo.0.clone()];
        assert_eq!(
            key(&repo.0, &dirs, &req),
            key(&repo.0, &moved, &req),
            "directory order is not an input"
        );
    }

    #[test]
    fn source_outside_the_repository_has_no_key() {
        let repo = Repo::new("outside-dir");
        let elsewhere = std::env::temp_dir();
        assert_eq!(key(&repo.0, &[elsewhere], &request()), None);
        assert_eq!(key(&repo.0, &[], &request()), None);
    }

    #[test]
    fn a_stored_document_reads_back_identical() {
        let repo = Repo::new("roundtrip");
        let json = repo.0.join("app.json");
        let text = r#"{"format_version":61,"index":{"0":{"name":"app"}}}"#;
        std::fs::write(&json, text).unwrap();
        let store = repo.0.join("store");
        std::fs::create_dir_all(&store).unwrap();
        assert_eq!(load_from(&store, "abc"), None);
        store_in(&store, "abc", &json);
        let expected: Value = serde_json::from_str(text).unwrap();
        assert_eq!(load_from(&store, "abc"), Some(expected));
    }

    #[test]
    fn pruning_drops_the_least_recently_used_beyond_the_cap() {
        let dir = std::env::temp_dir().join(format!("soothfast-rdprune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        for (name, age) in [("old", 30), ("mid", 20), ("new", 10)] {
            let path = dir.join(name);
            std::fs::write(&path, [0u8; 10]).unwrap();
            let file = File::options().write(true).open(&path).unwrap();
            file.set_modified(now - std::time::Duration::from_secs(age))
                .unwrap();
        }
        prune(&dir, 20);
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["mid", "new"]);
        prune(&dir, 0);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
