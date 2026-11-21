//! What the on-disk stores share: a cheap key over the tree, the manifest of
//! files and variables a build read, and entries named for both.
//!
//! Lookup is in two levels, as a compiler cache does it. A cheap key names the
//! toolchain, flags, lockfile and the files git lists in the package
//! directories. It selects a manifest of the files and variables the last
//! build read (from cargo and rustc dep-info). An entry is named for the
//! key and a digest of those inputs, so any of them changing is a miss, in
//! any checkout, including the merge-base worktree.
//!
//! The key also carries a stable half: the head, the variables and the
//! files that configure the build (`Cargo.toml`, `Cargo.lock`, `build.rs`,
//! cargo config, toolchain files), which a manifest does not cover. When a
//! lookup misses, the last entry stored under the same stable half is checked
//! against its manifest. If its inputs all still hold it is a hit, and is kept
//! under the current key too. A build that read a file the tree has since
//! changed misses either way.
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

pub mod pointer;

/// Variable families that change what rustc and rustdoc compile.
const ENV_PREFIXES: [&str; 4] = [
    "CARGO_ENCODED_",
    "CARGO_BUILD_",
    "CARGO_PROFILE_",
    "CARGO_TARGET_",
];

/// Single variables that change what rustc and rustdoc compile.
const ENV_NAMES: [&str; 2] = ["RUSTFLAGS", "RUSTDOCFLAGS"];

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

/// The files and variables a build read, in the package directories and
/// beyond. Paths are relative to the workspace root when they lie under it.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Inputs {
    pub files: BTreeSet<String>,
    pub env: BTreeSet<String>,
    /// The build may have read more than is listed here (a workspace build
    /// script that names nothing to watch, a dep-info that could not be
    /// read), so only the key it was built under finds its entry.
    pub opaque: bool,
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
            opaque: false,
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

/// `rustc [+toolchain] -vV` run in `dir`, which names the release, commit and
/// host of the compiler cargo would pick there.
pub fn rustc_version(toolchain: Option<&str>, dir: &Path) -> Option<String> {
    type Seen = Vec<((Option<String>, PathBuf), Option<String>)>;
    static SEEN: Mutex<Seen> = Mutex::new(Vec::new());
    let at = (toolchain.map(str::to_string), dir.to_path_buf());
    let mut seen = SEEN.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, version)) = seen.iter().find(|(k, _)| *k == at) {
        return version.clone();
    }
    let mut cmd = Command::new("rustc");
    if let Some(toolchain) = toolchain {
        cmd.arg(format!("+{toolchain}"));
    }
    let version = cmd
        .arg("-vV")
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    seen.push((at, version.clone()));
    version
}

/// The set variables that change what rustc and rustdoc compile, and those
/// of `extra` the caller also depends on.
pub fn build_env(extra: &[&str]) -> Vec<(String, String)> {
    select_env(
        std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))),
        extra,
    )
}

fn select_env(
    vars: impl Iterator<Item = (String, String)>,
    extra: &[&str],
) -> Vec<(String, String)> {
    let mut picked: Vec<_> = vars
        .filter(|(name, _)| {
            ENV_NAMES.contains(&name.as_str())
                || extra.contains(&name.as_str())
                || (ENV_PREFIXES.iter().any(|p| name.starts_with(p))
                    && !ENV_IGNORED.contains(&name.as_str()))
        })
        .collect();
    picked.sort();
    picked
}

/// The key for a build described by `head` (one line naming what the caller
/// builds) and `env`, from the tree at `root` whose package sources live in
/// `crate_dirs`. `None` when part of the input cannot be read.
pub fn cheap_key(
    root: &Path,
    crate_dirs: &[PathBuf],
    head: &str,
    env: &[(String, String)],
) -> Option<String> {
    cheap_key_skipping(root, crate_dirs, head, env, |_| false)
}

/// `cheap_key` leaving out the listed files that `skip` names. A skipped file
/// still counts if the build read it, through the manifest. The result is the
/// cheap half and the stable half joined by `_`.
pub fn cheap_key_skipping(
    root: &Path,
    crate_dirs: &[PathBuf],
    head: &str,
    env: &[(String, String)],
    skip: fn(&Path) -> bool,
) -> Option<String> {
    if crate_dirs.is_empty() {
        return None;
    }
    let rels = crate_dirs
        .iter()
        .map(|d| d.strip_prefix(root).ok().map(Path::to_path_buf))
        .collect::<Option<Vec<_>>>()?;

    let mut files: BTreeSet<PathBuf> = tracked_files(root, &rels)?
        .into_iter()
        .filter(|f| !is_scratch(f) && !f.starts_with(".github") && !skip(f))
        .collect();
    files.extend(ROOT_FILES.iter().map(PathBuf::from));

    let mut input = format!("{head}\n");
    for (name, value) in env {
        input.push_str(&format!("{name}={value}\n"));
    }
    let mut stable = input.clone();
    for file in files {
        let line = format!("{}\u{1}{}\n", file.display(), hash_path(&root.join(&file))?);
        input.push_str(&line);
        if configures_build(&file) {
            stable.push_str(&line);
        }
    }
    Some(format!(
        "{:016x}{KEY_SEP}{:016x}",
        fnv1a(input.as_bytes()),
        fnv1a(stable.as_bytes())
    ))
}

/// Separates the cheap half of a key from the stable half.
const KEY_SEP: char = '_';

/// The cheap half of `key`, which names its entries, and the stable half when
/// it has one.
fn halves(key: &str) -> (&str, Option<&str>) {
    match key.split_once(KEY_SEP) {
        Some((cheap, stable)) => (cheap, Some(stable)),
        None => (key, None),
    }
}

/// The part of `key` its entries are named for.
pub fn entry_key(key: &str) -> &str {
    halves(key).0
}

/// Whether `file` shapes a build in a way no manifest shows: cargo reads it
/// without recording it, or (`build.rs`) builds differently once it exists.
fn configures_build(file: &Path) -> bool {
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let in_cargo_dir = file
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|d| d == ".cargo");
    matches!(
        name,
        "Cargo.toml" | "Cargo.lock" | "build.rs" | "rust-toolchain" | "rust-toolchain.toml"
    ) || (in_cargo_dir && matches!(name, "config" | "config.toml"))
}

/// Whether `file` lies under a top-level `.soothfast*` path: the store and
/// the directories the CI action makes in the workspace while a job runs.
pub(crate) fn is_scratch(file: &Path) -> bool {
    file.components()
        .next()
        .is_some_and(|c| c.as_os_str().to_string_lossy().starts_with(".soothfast"))
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

fn entry_path(dir: &Path, key: &str, digest: &str, ext: &str) -> PathBuf {
    dir.join(format!("{key}-{digest}.{ext}"))
}

/// The entry stored under `key` whose inputs still hold what they did, or
/// else the last entry stored under the key's stable half if its inputs still
/// hold. That one is then kept under `key` as well. The entry's modification
/// time is renewed, which is what pruning goes by.
pub fn lookup(dir: &Path, key: &str, root: &Path, ext: &str) -> Option<PathBuf> {
    let (cheap, stable) = halves(key);
    if let Some(hit) = lookup_exact(dir, cheap, root, ext) {
        if let Some(stable) = stable {
            pointer::repoint(dir, cheap, stable);
        }
        return Some(hit);
    }
    let stable = stable?;
    let (prior, born) = pointer::latest(dir, stable).filter(|(prior, _)| prior != cheap)?;
    let hit = lookup_exact(dir, &prior, root, ext)?;
    let to = reindex(dir, &prior, cheap, &hit)?;
    pointer::remember(dir, cheap, stable, born);
    Some(to)
}

/// The entry named for `cheap` whose inputs still hold what they did.
pub fn lookup_exact(dir: &Path, cheap: &str, root: &Path, ext: &str) -> Option<PathBuf> {
    let manifest = std::fs::read_to_string(dir.join(format!("{cheap}.manifest"))).ok()?;
    let inputs = Inputs::from_json(&serde_json::from_str(&manifest).ok()?)?;
    let path = entry_path(dir, cheap, &inputs.digest(root)?, ext);
    let file = File::open(&path).ok()?;
    let _ = file.set_modified(SystemTime::now());
    Some(path)
}

/// Link every file of the entry `hit`, stored under the cheap key `from`, under
/// `cheap`, the payload last so a reader never finds half an entry. Returns the
/// new payload.
fn reindex(dir: &Path, from: &str, cheap: &str, hit: &Path) -> Option<PathBuf> {
    let payload = hit.file_name()?.to_str()?;
    let renamed = |name: &str| Some(format!("{cheap}{}", name.strip_prefix(from)?));
    let beside = format!("{payload}.");
    let own = [format!("{from}.manifest"), format!("{from}.group")];
    for file in std::fs::read_dir(dir).ok()?.flatten() {
        let name = file.file_name().to_string_lossy().into_owned();
        let kept_beside = name.starts_with(&beside) && !name.ends_with(".tmp");
        if (kept_beside || own.contains(&name))
            && let Some(to) = renamed(&name)
        {
            link_atomic(&file.path(), &dir.join(to));
        }
    }
    let to = dir.join(renamed(payload)?);
    link_atomic(hit, &to).then_some(to)
}

/// Keep `from` as the entry for `key`, unless an input was edited after the
/// build `started`.
pub fn insert(
    dir: &Path,
    key: &str,
    root: &Path,
    inputs: &Inputs,
    started: SystemTime,
    from: &Path,
    ext: &str,
) -> Option<PathBuf> {
    if inputs.changed_since(root, started) {
        return None;
    }
    let (cheap, stable) = halves(key);
    let digest = inputs.digest(root)?;
    let manifest = dir.join(format!("{cheap}.manifest"));
    let path = entry_path(dir, cheap, &digest, ext);
    let stored = copy_atomic(from, &path)
        && write_atomic(&manifest, inputs.to_json().to_string().as_bytes());
    if stored
        && !inputs.opaque
        && let Some(stable) = stable
    {
        pointer::remember(dir, cheap, stable, pointer::now());
    }
    stored.then_some(path)
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

pub fn write_atomic(path: &Path, bytes: &[u8]) -> bool {
    let tmp = temp_for(path);
    std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, path).is_ok()
}

fn link_atomic(from: &Path, to: &Path) -> bool {
    let tmp = temp_for(to);
    (std::fs::hard_link(from, &tmp).is_ok() || std::fs::copy(from, &tmp).is_ok())
        && std::fs::rename(&tmp, to).is_ok()
}

pub fn copy_atomic(from: &Path, to: &Path) -> bool {
    let tmp = temp_for(to);
    std::fs::copy(from, &tmp).is_ok() && std::fs::rename(&tmp, to).is_ok()
}

/// Whether `name` is what an entry holds, as opposed to a file kept with it.
fn is_payload(name: &str) -> bool {
    name.ends_with(".bin") || name.ends_with(".json")
}

/// The entry a file of the store belongs to: the key it is named for.
fn entry_of(name: &str) -> &str {
    name.split(['-', '.']).next().unwrap_or(name)
}

/// Remove the least recently used entries of `dir` beyond `max_bytes`,
/// always keeping the newest. An entry is every file named for one key, and
/// its age is that of its newest payload, so a manifest or a file kept beside
/// the payload never outlives it. Files being written (`*.tmp`) are left alone.
pub fn prune(dir: &Path, max_bytes: u64) {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return;
    };
    type Entry = (Option<SystemTime>, SystemTime, u64, Vec<PathBuf>);
    let mut entries: std::collections::BTreeMap<String, Entry> = Default::default();
    for e in listing.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(meta) = e.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if path.extension().is_some_and(|x| x == "tmp") {
            continue;
        }
        let entry =
            entries
                .entry(entry_of(&name).to_string())
                .or_insert((None, modified, 0, Vec::new()));
        if is_payload(&name) {
            entry.0 = entry.0.max(Some(modified));
        }
        entry.1 = entry.1.max(modified);
        entry.2 += meta.len();
        entry.3.push(path);
    }
    let mut entries: Vec<_> = entries.into_values().collect();
    entries.sort_by_key(|(payload, any, _, _)| std::cmp::Reverse(payload.unwrap_or(*any)));
    let newest = entries.iter().position(|e| e.0.is_some()).unwrap_or(0);
    let mut kept = 0;
    for (i, (_, _, len, files)) in entries.into_iter().enumerate() {
        kept += len;
        if i != newest && kept > max_bytes {
            for file in files {
                let _ = std::fs::remove_file(file);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub struct Repo(pub PathBuf);

    impl Repo {
        pub fn new(tag: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("soothfast-store-{tag}-{}", std::process::id()));
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

        pub fn git(&self, args: &[&str]) {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&self.0)
                .status()
                .unwrap()
                .success();
            assert!(ok);
        }

        pub fn write(&self, path: &str, text: &str) {
            let full = self.0.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }

        pub fn dirs(&self) -> [PathBuf; 2] {
            [self.0.join("app"), self.0.join("dep")]
        }

        pub fn store_dir(&self) -> PathBuf {
            let store = self.0.join("store");
            std::fs::create_dir_all(&store).unwrap();
            store
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub fn inputs(files: &[&str]) -> Inputs {
        Inputs {
            files: files.iter().map(|f| f.to_string()).collect(),
            env: BTreeSet::new(),
            opaque: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::testing::{Repo, inputs};
    use super::*;

    fn key(repo: &Repo, head: &str) -> String {
        cheap_key(&repo.0, &repo.dirs(), head, &[]).unwrap()
    }

    #[test]
    fn the_key_is_stable_and_follows_the_package_sources() {
        let repo = Repo::new("sources");
        let before = key(&repo, "h");
        assert_eq!(before, key(&repo, "h"));
        assert_ne!(before, key(&repo, "other"));
        repo.write("app/src/lib.rs", "pub fn app() -> u8 { 0 }\n");
        let after_app = key(&repo, "h");
        assert_ne!(before, after_app);
        repo.write("dep/src/lib.rs", "pub fn dep() -> u8 { 0 }\n");
        assert_ne!(after_app, key(&repo, "h"));
    }

    #[test]
    fn the_key_follows_the_lockfile_and_workspace_manifest() {
        let repo = Repo::new("lock");
        let before = key(&repo, "h");
        repo.write("Cargo.lock", "# lock 2\n");
        let relocked = key(&repo, "h");
        assert_ne!(before, relocked);
        repo.write("Cargo.toml", "[workspace]\nresolver = \"3\"\n");
        assert_ne!(relocked, key(&repo, "h"));
    }

    #[test]
    fn the_env_in_the_key_does_not_depend_on_how_the_cli_was_started() {
        let vars = |extra: &[(&str, &str)]| {
            let all = [("RUSTFLAGS", "-O"), ("CARGO_INCREMENTAL", "0")]
                .iter()
                .chain(extra)
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<Vec<_>>();
            select_env(all.into_iter(), &crate::binstore::ENV_NAMES)
        };
        let direct = vars(&[]);
        let under_cargo = vars(&[
            ("RUSTUP_TOOLCHAIN", "nightly-x86_64-unknown-linux-gnu"),
            ("CARGO", "/x/cargo"),
            ("CARGO_HOME", "/h/.cargo"),
        ]);
        assert_eq!(direct, under_cargo);
        assert_eq!(direct.len(), 2);
    }

    #[test]
    fn the_key_ignores_files_outside_the_packages() {
        let repo = Repo::new("outside");
        let before = key(&repo, "h");
        repo.write("README.md", "edited\n");
        repo.write("other/src/lib.rs", "pub fn other() -> u8 { 0 }\n");
        assert_eq!(before, key(&repo, "h"));

        let dirs = [repo.0.clone()];
        let root_package = cheap_key(&repo.0, &dirs, "h", &[]).unwrap();
        repo.write(".soothfast/rustdoc/entry.json", "{}");
        repo.write(".soothfast-action/Cargo.toml", "[package]\n");
        repo.write(".soothfast-branch/runs/a.json", "{}");
        repo.write(".github/workflows/ci.yml", "on: push\n");
        assert_eq!(root_package, cheap_key(&repo.0, &dirs, "h", &[]).unwrap());
        repo.write("README.md", "edited again\n");
        assert_ne!(root_package, cheap_key(&repo.0, &dirs, "h", &[]).unwrap());
    }

    #[test]
    fn a_skipped_file_is_left_out_of_the_key() {
        let repo = Repo::new("skip");
        let skip_notes: fn(&Path) -> bool = |f| f.extension().is_some_and(|x| x == "notes");
        let key = |skip| cheap_key_skipping(&repo.0, &repo.dirs(), "h", &[], skip).unwrap();
        repo.write("app/a.notes", "one\n");
        let before = key(skip_notes);
        repo.write("app/a.notes", "two\n");
        assert_eq!(before, key(skip_notes));
        let unskipped = key(|_| false);
        repo.write("app/a.notes", "three\n");
        assert_ne!(unskipped, key(|_| false));
    }

    #[test]
    fn the_key_follows_the_environment() {
        let repo = Repo::new("env");
        let flags = [("RUSTFLAGS".to_string(), "--cfg x".to_string())];
        let plain = key(&repo, "h");
        assert_ne!(
            plain,
            cheap_key(&repo.0, &repo.dirs(), "h", &flags).unwrap()
        );
    }

    #[test]
    fn every_cargo_flag_family_is_part_of_the_environment() {
        let vars = [
            ("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS", "-Cx"),
            ("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTDOCFLAGS", "-Cy"),
            ("CARGO_PROFILE_DEV_OPT_LEVEL", "3"),
            ("CARGO_BUILD_RUSTFLAGS", "-Cz"),
            ("CARGO_ENCODED_RUSTFLAGS", "a\u{1f}b"),
            ("CARGO_INCREMENTAL", "1"),
            ("RUSTC_WRAPPER", "sccache"),
            ("RUSTFLAGS", "-Cw"),
            ("RUSTDOCFLAGS", "-Cv"),
            ("CARGO_TARGET_DIR", "/t"),
            ("CARGO_BUILD_JOBS", "4"),
            ("HOME", "/home/x"),
        ];
        let picked = select_env(
            vars.iter().map(|(k, v)| (k.to_string(), v.to_string())),
            &["RUSTC_WRAPPER", "CARGO_INCREMENTAL"],
        );
        let names: Vec<_> = picked.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            [
                "CARGO_BUILD_RUSTFLAGS",
                "CARGO_ENCODED_RUSTFLAGS",
                "CARGO_INCREMENTAL",
                "CARGO_PROFILE_DEV_OPT_LEVEL",
                "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTDOCFLAGS",
                "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
                "RUSTC_WRAPPER",
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
        let before = key(&repo, "h");
        repo.write("app/vendor/lib/src/lib.rs", "pub fn v() -> u8 { 0 }\n");
        assert_ne!(before, key(&repo, "h"));
    }

    #[test]
    fn source_outside_the_repository_has_no_key() {
        let repo = Repo::new("outside-dir");
        let elsewhere = std::env::temp_dir();
        assert_eq!(cheap_key(&repo.0, &[elsewhere], "h", &[]), None);
        assert_eq!(cheap_key(&repo.0, &[], "h", &[]), None);
    }

    #[test]
    fn a_file_outside_the_package_read_by_the_build_changes_the_entry() {
        let repo = Repo::new("manifest");
        repo.write("docs/guide.md", "one\n");
        let from = repo.0.join("out");
        std::fs::write(&from, "bytes").unwrap();
        let store = repo.store_dir();
        let read = inputs(&["docs/guide.md"]);
        let future = SystemTime::now() + Duration::from_secs(3600);
        insert(&store, "k", &repo.0, &read, future, &from, "x").unwrap();
        assert!(lookup(&store, "k", &repo.0, "x").is_some());
        repo.write("docs/guide.md", "two\n");
        assert_eq!(lookup(&store, "k", &repo.0, "x"), None);
        repo.write("docs/guide.md", "one\n");
        assert!(lookup(&store, "k", &repo.0, "x").is_some());
    }

    const READ: [&str; 3] = ["app/src/lib.rs", "dep/src/lib.rs", "docs/guide.md"];

    fn stored(repo: &Repo, store: &Path, key: &str, read: &Inputs) {
        let from = repo.0.join("out");
        std::fs::write(&from, "bytes").unwrap();
        let future = SystemTime::now() + Duration::from_secs(3600);
        insert(store, key, &repo.0, read, future, &from, "x").unwrap();
    }

    fn setup(tag: &str) -> (Repo, PathBuf, String) {
        let repo = Repo::new(tag);
        repo.write("docs/guide.md", "guide\n");
        repo.write("app/soothfast-gate.lock", "one\n");
        let store = repo.store_dir();
        let first = key(&repo, "h");
        stored(&repo, &store, &first, &inputs(&READ));
        (repo, store, first)
    }

    #[test]
    fn a_file_the_build_never_read_changing_is_a_hit_kept_under_the_new_key() {
        let (repo, store, first) = setup("unrelated");
        repo.write("app/soothfast-gate.lock", "two\n");
        repo.write("app/CHANGELOG.md", "# changes\n");
        repo.write("app/openapi.yaml", "openapi: 3.1.0\n");
        let second = key(&repo, "h");
        assert_ne!(first, second);
        assert_eq!(lookup_exact(&store, entry_key(&second), &repo.0, "x"), None);

        let hit = lookup(&store, &second, &repo.0, "x").unwrap();
        assert!(hit.to_string_lossy().contains(entry_key(&second)));
        assert_eq!(std::fs::read(&hit).unwrap(), b"bytes");
        assert!(lookup_exact(&store, entry_key(&second), &repo.0, "x").is_some());
    }

    #[test]
    fn a_file_the_build_read_changing_is_a_miss() {
        for (file, text) in [
            ("app/src/lib.rs", "pub fn app() -> u8 { 0 }\n"),
            ("dep/src/lib.rs", "pub fn dep() -> u8 { 0 }\n"),
            ("docs/guide.md", "edited\n"),
        ] {
            let (repo, store, _) = setup("read");
            repo.write("app/soothfast-gate.lock", "two\n");
            repo.write(file, text);
            assert_eq!(
                lookup(&store, &key(&repo, "h"), &repo.0, "x"),
                None,
                "{file}"
            );
        }
    }

    #[test]
    fn a_deleted_input_is_a_miss() {
        let (repo, store, _) = setup("deleted");
        repo.write("app/soothfast-gate.lock", "two\n");
        std::fs::remove_file(repo.0.join("docs/guide.md")).unwrap();
        assert_eq!(lookup(&store, &key(&repo, "h"), &repo.0, "x"), None);
    }

    #[test]
    fn a_file_added_under_an_input_directory_is_a_miss() {
        let repo = Repo::new("added");
        repo.write("gen/a.txt", "a\n");
        let store = repo.store_dir();
        let read = inputs(&["app/src/lib.rs", "gen"]);
        stored(&repo, &store, &key(&repo, "h"), &read);
        repo.write("gen/b.txt", "b\n");
        repo.write("app/soothfast-gate.lock", "two\n");
        assert_eq!(lookup(&store, &key(&repo, "h"), &repo.0, "x"), None);
    }

    #[test]
    fn what_configures_the_build_or_names_it_never_falls_back() {
        let (repo, store, _) = setup("config");
        repo.write("app/soothfast-gate.lock", "two\n");
        assert!(lookup(&store, &key(&repo, "h"), &repo.0, "x").is_some());
        assert_eq!(lookup(&store, &key(&repo, "other"), &repo.0, "x"), None);
        let flags = [("RUSTFLAGS".to_string(), "--cfg x".to_string())];
        let flagged = cheap_key(&repo.0, &repo.dirs(), "h", &flags).unwrap();
        assert_eq!(lookup(&store, &flagged, &repo.0, "x"), None);
        for file in [
            "Cargo.lock",
            "Cargo.toml",
            "app/Cargo.toml",
            "app/.cargo/config.toml",
        ] {
            let (repo, store, _) = setup("config-file");
            repo.write(file, "# changed\n");
            assert_eq!(
                lookup(&store, &key(&repo, "h"), &repo.0, "x"),
                None,
                "{file}"
            );
        }
    }

    #[test]
    fn an_opaque_build_only_hits_on_its_own_key() {
        let repo = Repo::new("opaque");
        let store = repo.store_dir();
        let first = key(&repo, "h");
        let opaque = Inputs {
            opaque: true,
            ..inputs(&["app/src/lib.rs"])
        };
        stored(&repo, &store, &first, &opaque);
        assert!(lookup(&store, &first, &repo.0, "x").is_some());
        repo.write("app/soothfast-gate.lock", "two\n");
        assert_eq!(lookup(&store, &key(&repo, "h"), &repo.0, "x"), None);
    }

    #[test]
    fn a_torn_or_stale_pointer_is_a_miss() {
        let (repo, store, _) = setup("pointer");
        repo.write("app/soothfast-gate.lock", "two\n");
        let second = key(&repo, "h");
        let pointer = store.join(format!("{}.latest", halves(&second).1.unwrap()));
        for text in ["", "nonsense", "0000000000000000"] {
            std::fs::write(&pointer, text).unwrap();
            assert_eq!(lookup(&store, &second, &repo.0, "x"), None, "{text:?}");
        }
    }

    #[test]
    fn a_pointer_out_of_the_store_or_to_another_build_is_a_miss() {
        let (repo, store, first) = setup("escape");
        let other = key(&repo, "other");
        stored(&repo, &store, &other, &inputs(&READ));
        repo.write("app/soothfast-gate.lock", "two\n");
        let second = key(&repo, "h");
        let pointer = store.join(format!("{}.latest", halves(&second).1.unwrap()));

        let outside = repo.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        for file in std::fs::read_dir(&store).unwrap().flatten() {
            std::fs::copy(file.path(), outside.join(file.file_name())).unwrap();
        }
        let cheap = entry_key(&first);
        let absolute = outside.join(cheap).to_string_lossy().into_owned();
        for text in [format!("../outside/{cheap}"), absolute] {
            std::fs::write(&pointer, &text).unwrap();
            assert_eq!(lookup(&store, &second, &repo.0, "x"), None, "{text}");
        }

        std::fs::write(&pointer, entry_key(&other)).unwrap();
        assert_eq!(lookup(&store, &second, &repo.0, "x"), None);
        let before = std::fs::read_dir(&outside).unwrap().count();
        std::fs::write(&pointer, cheap).unwrap();
        assert!(lookup(&store, &second, &repo.0, "x").is_some());
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), before);
    }

    #[test]
    fn a_build_script_appearing_is_a_miss() {
        let (repo, store, _) = setup("buildrs");
        repo.write("app/soothfast-gate.lock", "two\n");
        repo.write("dep/build.rs", "fn main() {}\n");
        assert_eq!(lookup(&store, &key(&repo, "h"), &repo.0, "x"), None);
    }

    #[test]
    fn an_exact_hit_repoints_the_stable_half() {
        let (repo, store, first) = setup("repoint");
        repo.write("app/src/lib.rs", "pub fn app() -> u8 { 0 }\n");
        stored(&repo, &store, &key(&repo, "h"), &inputs(&READ));
        repo.write("app/src/lib.rs", "pub fn app() {}\n");
        assert_eq!(key(&repo, "h"), first);
        assert!(lookup(&store, &first, &repo.0, "x").is_some());
        repo.write("app/soothfast-gate.lock", "two\n");
        assert!(lookup(&store, &key(&repo, "h"), &repo.0, "x").is_some());
    }

    #[test]
    fn an_exact_hit_on_an_opaque_entry_leaves_no_pointer() {
        let repo = Repo::new("opaque-hit");
        let store = repo.store_dir();
        let opaque = Inputs {
            opaque: true,
            ..inputs(&["app/src/lib.rs"])
        };
        stored(&repo, &store, &key(&repo, "h"), &opaque);
        assert!(lookup(&store, &key(&repo, "h"), &repo.0, "x").is_some());
        let pointers = std::fs::read_dir(&store)
            .unwrap()
            .flatten()
            .filter(|f| f.path().extension().is_some_and(|x| x == "latest"))
            .count();
        assert_eq!(pointers, 0);
    }

    #[test]
    fn pruning_keeps_the_newest_payload_over_a_newer_pointer() {
        let repo = Repo::new("prune-pointer");
        let store = repo.store_dir();
        let first = key(&repo, "h");
        let from = repo.0.join("out");
        std::fs::write(&from, "bytes").unwrap();
        let future = SystemTime::now() + Duration::from_secs(3600);
        let read = inputs(&["app/src/lib.rs"]);
        insert(&store, &first, &repo.0, &read, future, &from, "json").unwrap();
        prune(&store, 0);
        let left: Vec<_> = std::fs::read_dir(&store)
            .unwrap()
            .flatten()
            .map(|f| f.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(left.iter().any(|n| n.starts_with(entry_key(&first))));
    }

    #[test]
    fn an_entry_without_a_pointer_is_found_only_by_its_own_key() {
        let (repo, store, first) = setup("legacy");
        for file in std::fs::read_dir(&store).unwrap().flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            if name.ends_with(".latest") || name.ends_with(".stable") {
                std::fs::remove_file(file.path()).unwrap();
            }
        }
        assert!(lookup(&store, &first, &repo.0, "x").is_some());
        repo.write("app/soothfast-gate.lock", "two\n");
        assert_eq!(lookup(&store, &key(&repo, "h"), &repo.0, "x"), None);
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
        let from = repo.0.join("out");
        std::fs::write(&from, "bytes").unwrap();
        let store = repo.store_dir();
        let read = inputs(&["docs/guide.md"]);
        let before_edit = SystemTime::now() - Duration::from_secs(3600);
        assert_eq!(
            insert(&store, "k", &repo.0, &read, before_edit, &from, "x"),
            None
        );
        assert_eq!(lookup(&store, "k", &repo.0, "x"), None);
    }

    #[test]
    fn concurrent_stores_leave_one_readable_entry() {
        let repo = Repo::new("concurrent");
        repo.write("docs/guide.md", "one\n");
        let from = repo.0.join("out");
        let bytes = vec![7u8; 1 << 20];
        std::fs::write(&from, &bytes).unwrap();
        let store = repo.store_dir();
        let read = inputs(&["docs/guide.md"]);
        let future = SystemTime::now() + Duration::from_secs(3600);
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| insert(&store, "k", &repo.0, &read, future, &from, "x"));
            }
        });
        let path = lookup(&store, "k", &repo.0, "x").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        let temps = std::fs::read_dir(&store)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"))
            .count();
        assert_eq!(temps, 0);
    }

    #[test]
    fn pruning_removes_an_entry_whole_by_the_age_of_its_payload() {
        let dir =
            std::env::temp_dir().join(format!("soothfast-prune-entry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        for (name, age) in [
            ("a.manifest", 1),
            ("a-1.bin", 50),
            ("a-1.bin.digest", 1),
            ("b.manifest", 2),
            ("b-1.bin", 10),
        ] {
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
        assert_eq!(left, ["b-1.bin", "b.manifest"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pruning_drops_the_least_recently_used_beyond_the_cap_and_spares_temp_files() {
        let dir = std::env::temp_dir().join(format!("soothfast-prune-{}", std::process::id()));
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
