//! Built bench executables kept on disk, keyed by everything that produces
//! their bytes. The key and the entries follow `store`.
//!
//! The run cache, the item cache and the link maps all name a binary by its
//! digest, so an entry must be the bytes cargo would write or be a miss. Two
//! things beyond the sources reach those bytes: the checkout path (through
//! `env!("CARGO_MANIFEST_DIR")`, `file!()` and debug info) and the target
//! directory (through `OUT_DIR`). Both are part of the key, so a tree built at
//! another path misses.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::runcache;
use crate::store::{self, Inputs};

/// Bytes kept before the least recently used entries are dropped.
pub(crate) const MAX_BYTES: u64 = 1 << 29;

/// Variables a bench build depends on beyond the ones every compile does.
pub const ENV_NAMES: [&str; 5] = [
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTUP_TOOLCHAIN",
    "CARGO_INCREMENTAL",
];

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
    store::cheap_key_skipping(root, crate_dirs, &head, req.env, is_markdown)
}

/// Markdown stays out of the cheap key so that a docs-only change is a hit.
/// A build reads one only through `include_str!` and the like, which the
/// manifest records.
fn is_markdown(file: &Path) -> bool {
    file.extension()
        .is_some_and(|x| x == "md" || x == "markdown")
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
/// The entry replaces any older one of its `group`. Returns the entry it made.
#[allow(clippy::too_many_arguments)]
pub fn store(
    dir: &Path,
    key: &str,
    key_now: Option<String>,
    root: &Path,
    inputs: &Inputs,
    started: SystemTime,
    exe: &Path,
    group: &str,
) -> Option<PathBuf> {
    if key_now.as_deref() != Some(key) || load(dir, key, root).is_some() {
        return None;
    }
    let entry = store::insert(dir, key, root, inputs, started, exe, "bin")?;
    let born = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    store::write_atomic(
        &dir.join(format!("{key}.group")),
        format!("{born}\n{group}").as_bytes(),
    );
    store::write_atomic(&link(exe), entry.as_os_str().as_encoded_bytes());
    keep_newest(dir);
    store::prune(dir, MAX_BYTES);
    Some(entry)
}

/// Remove every executable but the newest of its group, with the files kept
/// beside it. A cache holds the binary a branch's lineage runs, not its
/// history. Age is the time the entry was made, which a merge keeps.
pub fn keep_newest(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut groups: BTreeMap<String, Vec<(u128, PathBuf)>> = BTreeMap::new();
    for e in entries.flatten() {
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        let Some((key, _)) = name.split_once('-') else {
            continue;
        };
        if e.path().extension().is_none_or(|x| x != "bin") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(dir.join(format!("{key}.group"))) else {
            continue;
        };
        let Some((born, group)) = text.split_once('\n') else {
            continue;
        };
        let Ok(born) = born.parse() else { continue };
        groups
            .entry(group.to_string())
            .or_default()
            .push((born, e.path()));
    }
    for mut entries in groups.into_values() {
        entries.sort_by_key(|(born, _)| std::cmp::Reverse(*born));
        for (_, entry) in entries.into_iter().skip(1) {
            for ext in ["digest", "extents"] {
                let _ = std::fs::remove_file(sidecar(&entry, ext));
            }
            let _ = std::fs::remove_file(entry);
        }
    }
}

/// Whether `exe` lies in the store, so no build of it exists in a target
/// directory.
pub fn holds(exe: &Path) -> bool {
    dir().is_some_and(|d| exe.starts_with(d))
}

/// The file beside a built executable that names the entry made from it.
fn link(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".soothfast-entry");
    exe.with_file_name(name)
}

/// The entry holding the bytes of `exe`: itself when it lies in the store,
/// else the entry made from it, if that is still the same size.
pub fn entry_of(exe: &Path) -> Option<PathBuf> {
    if holds(exe) {
        return Some(exe.to_path_buf());
    }
    let entry = PathBuf::from(std::fs::read_to_string(link(exe)).ok()?);
    let same = std::fs::metadata(&entry).ok()?.len() == std::fs::metadata(exe).ok()?.len();
    same.then_some(entry)
}

/// What was worked out from the bytes of `exe` and kept with its entry.
pub fn attached(exe: &Path, ext: &str) -> Option<String> {
    read_sidecar(&entry_of(exe)?, ext)
}

/// Keep `body`, worked out from the bytes of `exe`, with its entry if it has
/// one and nothing is kept yet.
pub fn attach(exe: &Path, ext: &str, body: &str) {
    if let Some(entry) = entry_of(exe) {
        write_sidecar(&entry, ext, body);
    }
}

/// Entries are written once and never replaced under the same name, so a file
/// kept beside one stays true of it. Its size, written first, catches a file
/// left by an entry that has since been removed and made again.
fn sidecar(entry: &Path, ext: &str) -> PathBuf {
    let mut name = entry.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{ext}"));
    entry.with_file_name(name)
}

pub fn read_sidecar(entry: &Path, ext: &str) -> Option<String> {
    let text = std::fs::read_to_string(sidecar(entry, ext)).ok()?;
    let (len, body) = text.split_once('\n')?;
    let size = std::fs::metadata(entry).ok()?.len();
    (len.parse() == Ok(size)).then(|| body.to_string())
}

pub fn write_sidecar(entry: &Path, ext: &str, body: &str) {
    if !sidecar(entry, ext).exists() {
        replace_sidecar(entry, ext, body);
    }
}

/// Write the sidecar whether or not one is there.
pub fn replace_sidecar(entry: &Path, ext: &str, body: &str) {
    if let Ok(meta) = std::fs::metadata(entry) {
        store::write_atomic(
            &sidecar(entry, ext),
            format!("{}\n{body}", meta.len()).as_bytes(),
        );
    }
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
        repo.write("app/README.md", "docs only\n");
        repo.write("app/docs/guide.md", "docs only\n");
        repo.write("other/src/lib.rs", "pub fn other() -> u8 { 0 }\n");
        assert_eq!(base, key_with(&repo, &request()));
        repo.write("app/data.json", "{}\n");
        assert_ne!(base, key_with(&repo, &request()));
        let base = key_with(&repo, &request());
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
        store(
            &dir,
            "k",
            Some("k".into()),
            &repo.0,
            &read,
            future,
            &exe,
            "g",
        );
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
        let wrong = Some("other".into());
        assert_eq!(
            store(&dir, "k", wrong, &repo.0, &read, future, &exe, "g"),
            None
        );
        assert_eq!(load(&dir, "k", &repo.0), None);
    }

    fn put(repo: &Repo, dir: &Path, key: &str, group: &str, bytes: &[u8]) -> PathBuf {
        let exe = repo.0.join(format!("bench-{key}"));
        std::fs::write(&exe, bytes).unwrap();
        let future = SystemTime::now() + Duration::from_secs(3600);
        let entry = store(
            dir,
            key,
            Some(key.into()),
            &repo.0,
            &inputs(&[]),
            future,
            &exe,
            group,
        );
        entry.unwrap()
    }

    #[test]
    fn what_is_kept_beside_an_entry_reads_back_only_for_that_entry() {
        let repo = Repo::new("bin-sidecar");
        let dir = repo.store_dir();
        let entry = put(&repo, &dir, "k", "g", b"twelve bytes");
        assert_eq!(read_sidecar(&entry, "digest"), None);
        write_sidecar(&entry, "digest", "abc");
        assert_eq!(read_sidecar(&entry, "digest"), Some("abc".to_string()));
        write_sidecar(&entry, "digest", "other");
        assert_eq!(read_sidecar(&entry, "digest"), Some("abc".to_string()));

        std::fs::write(&entry, "longer than twelve bytes").unwrap();
        assert_eq!(read_sidecar(&entry, "digest"), None);
    }

    #[test]
    fn a_stored_executable_carries_its_digest_to_a_later_process() {
        let repo = Repo::new("bin-attach");
        let dir = repo.store_dir();
        let entry = put(&repo, &dir, "k", "g", b"bytes");
        let built = repo.0.join("bench-k");
        attach(&built, "digest", "d1");
        assert_eq!(read_sidecar(&entry, "digest"), Some("d1".to_string()));
        assert_eq!(attached(&built, "digest"), Some("d1".to_string()));
        assert_eq!(attached(&repo.0.join("elsewhere"), "digest"), None);
    }

    #[test]
    fn only_the_newest_executable_of_a_group_is_kept() {
        let repo = Repo::new("bin-group");
        let dir = repo.store_dir();
        let old = put(&repo, &dir, "a", "pkg", b"old");
        write_sidecar(&old, "extents", "x");
        let other = put(&repo, &dir, "b", "other", b"other");
        let past = SystemTime::now() - Duration::from_secs(60);
        for entry in [&old, &other] {
            let file = std::fs::File::options().write(true).open(entry).unwrap();
            file.set_modified(past).unwrap();
        }
        let new = put(&repo, &dir, "c", "pkg", b"new");
        assert!(!old.exists());
        assert!(!sidecar(&old, "extents").exists());
        assert!(new.exists());
        assert!(other.exists());
    }
}
