//! Built bench executables kept on disk, keyed by everything that produces
//! their bytes. The key and the entries follow `store`.
//!
//! The run cache, the item cache and the link maps all name a binary by its
//! digest, so an entry must be the bytes cargo would write or be a miss. Two
//! things beyond the sources reach those bytes: the checkout path (through
//! `env!("CARGO_MANIFEST_DIR")`, `file!()` and debug info) and the target
//! directory (through `OUT_DIR`). Both are part of the key, so a tree built at
//! another path misses.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::runcache;
use crate::store::{self, Inputs};

/// Bytes kept before the least recently used entries are dropped.
pub(crate) const MAX_BYTES: u64 = 1 << 30;

/// One bench build, as far as the key is concerned.
pub struct Request<'a> {
    pub pkg: &'a str,
    pub target: &'a str,
    pub profile: &'a str,
    pub features: Option<&'a str>,
    pub codegen_units: Option<&'a str>,
    pub toolchain: Option<&'a str>,
    /// `rustc -vV` of the compiler cargo picks.
    pub rustc: &'a str,
    pub target_dir: &'a Path,
    /// `CARGO_HOME`, where registry sources (and so their panic paths) live.
    pub cargo_home: &'a str,
    pub env: &'a [(String, String)],
}

/// The key for `req` built from the tree at `root`, whose package sources
/// live in `crate_dirs`. `None` when part of the input cannot be read.
pub fn key(root: &Path, crate_dirs: &[PathBuf], req: &Request) -> Option<String> {
    let head = format!(
        "bin1\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}",
        req.pkg,
        req.target,
        req.profile,
        req.features.unwrap_or(""),
        req.codegen_units.unwrap_or(""),
        req.toolchain.unwrap_or(""),
        req.rustc,
        root.display(),
        req.target_dir.display(),
        req.cargo_home,
    );
    store::cheap_key(root, crate_dirs, &head, req.env)
}

/// `.soothfast/bins`, created on demand.
pub fn dir() -> Option<PathBuf> {
    runcache::state_dir("bins")
}

/// The executable stored under `key` in `dir` whose inputs still hold what
/// they did.
pub fn load(dir: &Path, key: &str, root: &Path) -> Option<PathBuf> {
    store::lookup(dir, key, root, "bin")
}

/// Keep the executable cargo wrote at `exe` for a build that started at
/// `started`, unless the tree was edited meanwhile: `key` must still be the
/// key the tree computes (`key_now`), and no input may be newer than the build.
pub fn store(
    dir: &Path,
    key: &str,
    key_now: Option<String>,
    root: &Path,
    inputs: &Inputs,
    started: SystemTime,
    exe: &Path,
) {
    if key_now.as_deref() != Some(key) {
        return;
    }
    if store::insert(dir, key, root, inputs, started, exe, "bin").is_some() {
        store::prune(dir, MAX_BYTES);
    }
}

/// Whether `exe` lies in the store, so no build of it exists in a target
/// directory.
pub fn holds(exe: &Path) -> bool {
    dir().is_some_and(|d| exe.starts_with(d))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use super::*;
    use crate::store::testing::{Repo, inputs};

    const TARGET: &str = "/t";

    fn request() -> Request<'static> {
        Request {
            pkg: "app",
            target: "soothfast",
            profile: "bench",
            features: None,
            codegen_units: Some("1"),
            toolchain: None,
            rustc: "rustc 1.99.0",
            target_dir: Path::new(TARGET),
            cargo_home: "/home/x/.cargo",
            env: &[],
        }
    }

    fn key_with(repo: &Repo, req: &Request) -> String {
        key(&repo.0, &repo.dirs(), req).unwrap()
    }

    #[test]
    fn the_key_follows_every_input_of_the_build() {
        let repo = Repo::new("bin-request");
        let base = key_with(&repo, &request());
        assert_eq!(base, key_with(&repo, &request()));
        let flags = [("CARGO_PROFILE_BENCH_DEBUG".to_string(), "2".to_string())];
        let variants = [
            Request {
                pkg: "dep",
                ..request()
            },
            Request {
                target: "other",
                ..request()
            },
            Request {
                profile: "dev",
                ..request()
            },
            Request {
                features: Some("a"),
                ..request()
            },
            Request {
                codegen_units: None,
                ..request()
            },
            Request {
                toolchain: Some("nightly"),
                ..request()
            },
            Request {
                rustc: "rustc 1.99.1",
                ..request()
            },
            Request {
                target_dir: Path::new("/other"),
                ..request()
            },
            Request {
                cargo_home: "/root/.cargo",
                ..request()
            },
            Request {
                env: &flags,
                ..request()
            },
        ];
        let keys: BTreeSet<_> = variants.iter().map(|r| key_with(&repo, r)).collect();
        assert_eq!(keys.len(), variants.len());
        assert!(!keys.contains(&base));
    }

    #[test]
    fn a_source_edit_or_dependency_bump_changes_the_key_and_a_docs_edit_does_not() {
        let repo = Repo::new("bin-edits");
        let base = key_with(&repo, &request());
        repo.write("README.md", "docs only\n");
        repo.write("other/src/lib.rs", "pub fn other() -> u8 { 0 }\n");
        assert_eq!(base, key_with(&repo, &request()));
        repo.write("app/src/lib.rs", "pub fn app() -> u8 { 0 }\n");
        let edited = key_with(&repo, &request());
        assert_ne!(base, edited);
        repo.write("Cargo.lock", "# lock 2\n");
        assert_ne!(edited, key_with(&repo, &request()));
    }

    #[test]
    fn the_same_tree_at_another_path_has_another_key() {
        let one = Repo::new("bin-path-one");
        let two = Repo::new("bin-path-two");
        assert_ne!(key_with(&one, &request()), key_with(&two, &request()));
    }

    #[test]
    fn a_stored_executable_reads_back_identical_and_still_runnable() {
        use std::os::unix::fs::PermissionsExt;

        let repo = Repo::new("bin-roundtrip");
        repo.write("docs/guide.md", "one\n");
        let exe = repo.0.join("bench");
        let bytes: Vec<u8> = (0..=255u8).cycle().take(1 << 20).collect();
        std::fs::write(&exe, &bytes).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let dir = repo.store_dir();
        assert_eq!(store::lookup(&dir, "k", &repo.0, "bin"), None);

        let read = inputs(&["docs/guide.md"]);
        let future = SystemTime::now() + Duration::from_secs(3600);
        store(&dir, "k", Some("k".into()), &repo.0, &read, future, &exe);
        let hit = store::lookup(&dir, "k", &repo.0, "bin").unwrap();
        assert_eq!(std::fs::read(&hit).unwrap(), bytes);
        let mode = std::fs::metadata(&hit).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111);

        repo.write("docs/guide.md", "two\n");
        assert_eq!(store::lookup(&dir, "k", &repo.0, "bin"), None);
    }

    #[test]
    fn a_wrong_key_stores_nothing() {
        let repo = Repo::new("bin-wrongkey");
        let exe = repo.0.join("bench");
        std::fs::write(&exe, "x").unwrap();
        let future = SystemTime::now() + Duration::from_secs(3600);
        let read = inputs(&[]);
        let dir = repo.store_dir();
        store(
            &dir,
            "k",
            Some("other".into()),
            &repo.0,
            &read,
            future,
            &exe,
        );
        assert_eq!(load(&dir, "k", &repo.0), None);
    }
}
