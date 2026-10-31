//! Seed the merge-base worktree's target dir with HEAD's dependency
//! artifacts, so the base build compiles only what differs from HEAD.
//!
//! The two sides cannot share one target dir: a workspace member hashes to
//! the same `-C metadata` from either checkout (see
//! [`crate::invoke::worktree_target_dir`]). A dependency's artifacts carry
//! that hash too, in `.fingerprint/`, `build/` and `deps/`, so copying every
//! unit whose package is not a member lets cargo find the base side's
//! unchanged dependencies fresh, while a dependency that differs between the
//! two `Cargo.lock`s hashes differently and is built as usual.
//!
//! Cargo's newer layout has no `.fingerprint/` or `deps/`: a unit lives whole
//! in `build/<pkg>/<hash>/`, and that directory is what is copied.
//!
//! Files are copied, never hardlinked: through a shared inode, any in-place
//! write by the base build would change HEAD's artifacts between
//! measurements. `fs::copy` reflinks where the filesystem can. The
//! copies do not point back into HEAD's dir: cargo rewrites a build script's
//! recorded `OUT_DIR` to wherever its output now lives.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::workspace;

/// What one [`seed`] call copied.
#[derive(Debug, Default, PartialEq)]
pub struct Seeded {
    pub units: usize,
    pub bytes: u64,
    /// Each copied unit, as its `.fingerprint/` or `build/<pkg>/<hash>` name.
    pub names: Vec<String>,
    /// Units left for the base build to compile, each with why it could not
    /// be copied.
    pub skipped: Vec<String>,
}

/// Copy the units in profile dir `head` that belong to no package in
/// `members` into profile dir `base`, skipping units `base` already has.
pub fn seed(head: &Path, base: &Path, members: &BTreeSet<String>) -> io::Result<Seeded> {
    let mut seeded = Seeded::default();
    seed_flat(head, base, members, &mut seeded)?;
    seed_nested(head, base, members, &mut seeded)?;
    Ok(seeded)
}

/// The layout with `.fingerprint/`, `build/` and `deps/` side by side.
fn seed_flat(
    head: &Path,
    base: &Path,
    members: &BTreeSet<String>,
    seeded: &mut Seeded,
) -> io::Result<()> {
    let Ok(fingerprints) = fs::read_dir(head.join(".fingerprint")) else {
        return Ok(());
    };
    let mut units: BTreeMap<String, (String, Vec<PathBuf>)> = BTreeMap::new();
    for entry in fingerprints {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some((pkg, hash)) = split_unit(&name)
            && !members.contains(pkg)
            && fs::symlink_metadata(base.join(".fingerprint").join(&name)).is_err()
        {
            units.insert(hash.to_string(), (name, Vec::new()));
        }
    }
    if units.is_empty() {
        return Ok(());
    }
    for dir in ["build", "deps"] {
        let Ok(entries) = fs::read_dir(head.join(dir)) else {
            continue;
        };
        for entry in entries {
            let name = entry?.file_name().to_string_lossy().into_owned();
            let stem = name.split('.').next().unwrap_or(&name);
            if let Some((_, hash)) = split_unit(stem)
                && let Some((_, files)) = units.get_mut(hash)
            {
                files.push(Path::new(dir).join(&name));
            }
        }
    }
    // A unit's fingerprint goes last: a unit whose copy fails or is cut short
    // has none, so cargo rebuilds it instead of trusting a partial copy.
    for (fingerprint, mut files) in units.into_values() {
        files.push(Path::new(".fingerprint").join(&fingerprint));
        let copied = files.iter().try_fold(0, |bytes, rel| {
            Ok::<_, io::Error>(bytes + copy_tree(&head.join(rel), &base.join(rel))?)
        });
        match copied {
            Ok(bytes) => {
                seeded.units += 1;
                seeded.bytes += bytes;
                seeded.names.push(fingerprint);
            }
            Err(e) => seeded.skipped.push(format!("{fingerprint}: {e}")),
        }
    }
    Ok(())
}

/// The layout with one `build/<pkg>/<hash>/` directory per unit.
fn seed_nested(
    head: &Path,
    base: &Path,
    members: &BTreeSet<String>,
    seeded: &mut Seeded,
) -> io::Result<()> {
    let Ok(pkgs) = fs::read_dir(head.join("build")) else {
        return Ok(());
    };
    for pkg in pkgs {
        let pkg = match pkg {
            Ok(pkg) => pkg,
            Err(e) => {
                seeded.skipped.push(format!("build: {e}"));
                continue;
            }
        };
        let name = pkg.file_name().to_string_lossy().into_owned();
        if split_unit(&name).is_some() || members.contains(&name) || !pkg.path().is_dir() {
            continue;
        }
        let units = match fs::read_dir(pkg.path()) {
            Ok(units) => units,
            Err(e) => {
                seeded.skipped.push(format!("{name}: {e}"));
                continue;
            }
        };
        for unit in units {
            let unit = match unit {
                Ok(unit) => unit,
                Err(e) => {
                    seeded.skipped.push(format!("{name}: {e}"));
                    continue;
                }
            };
            let hash = unit.file_name().to_string_lossy().into_owned();
            let rel = Path::new("build").join(&name).join(&hash);
            if !is_hash(&hash) || fs::symlink_metadata(base.join(&rel)).is_ok() {
                continue;
            }
            match copy_unit(&head.join(&rel), &base.join(&rel)) {
                Ok(bytes) => {
                    seeded.units += 1;
                    seeded.bytes += bytes;
                    seeded.names.push(format!("{name}/{hash}"));
                }
                Err(e) => seeded.skipped.push(format!("{name}/{hash}: {e}")),
            }
        }
    }
    Ok(())
}

/// Copy a unit directory with its `fingerprint` last, for the reason a flat
/// unit's fingerprint goes last.
fn copy_unit(src: &Path, dst: &Path) -> io::Result<u64> {
    let mut bytes = 0;
    for last in [false, true] {
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            if (entry.file_name() == "fingerprint") == last {
                bytes += copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
            }
        }
    }
    Ok(bytes)
}

/// Seed the merge-base worktree's bench profile dir from HEAD's, which must
/// already be built. `wt_target` is [`crate::invoke::worktree_target_dir`],
/// inside HEAD's target dir. A failure only leaves the base build cold.
pub fn seed_worktree(wt: &Path, wt_target: &Path) {
    let start = Instant::now();
    let (Some(head_target), Ok(head_members), Ok(base_members)) = (
        wt_target.parent(),
        workspace::members(None),
        workspace::members(Some(wt)),
    ) else {
        return;
    };
    let members = head_members.into_iter().chain(base_members).collect();
    match seed(
        &head_target.join("release"),
        &wt_target.join("release"),
        &members,
    ) {
        Ok(s) => {
            let _ = fs::write(
                wt_target.join("release").join(SEEDED),
                s.names.iter().map(|n| format!("{n}\n")).collect::<String>(),
            );
            if s.units > 0 {
                println!(
                    "gate: seeded {} dependency units ({:.1} MiB) from HEAD's target dir in {:.1}s",
                    s.units,
                    s.bytes as f64 / (1024.0 * 1024.0),
                    start.elapsed().as_secs_f64()
                );
            }
            if let Some(first) = s.skipped.first() {
                println!(
                    "gate: {} dependency units not seeded and built cold, first {first}",
                    s.skipped.len()
                );
            }
        }
        Err(e) => println!(
            "gate: could not seed the merge-base target dir ({e}); its dependencies build cold"
        ),
    }
}

/// Where [`seed_worktree`] lists the units it copied, for [`report`].
const SEEDED: &str = ".soothfast-seeded";

/// After a failed merge-base build, name what the seed put in its target dir.
/// `SOOTHFAST_DEBUG_SEED=1` also lists the harness artifacts there.
pub fn report(wt_target: &Path) {
    let profile = wt_target.join("release");
    if let Ok(list) = fs::read_to_string(profile.join(SEEDED)) {
        for unit in list.lines() {
            eprintln!("gate: seeded unit {unit}");
        }
    }
    if std::env::var_os("SOOTHFAST_DEBUG_SEED").is_some_and(|v| v == "1") {
        for line in harness_artifacts(&profile) {
            eprintln!("gate: seed debug {line}");
        }
    }
}

/// `deps/libsoothfast*` and `.fingerprint/soothfast-*` in `profile`, each
/// with its size and mtime in seconds since the epoch.
fn harness_artifacts(profile: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for (dir, prefix) in [("deps", "libsoothfast"), (".fingerprint", "soothfast-")] {
        let Ok(entries) = fs::read_dir(profile.join(dir)) else {
            out.push(format!("{dir}: unreadable"));
            continue;
        };
        let mut names: Vec<_> = entries
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
            .collect();
        names.sort_by_key(|e| e.file_name());
        for entry in names {
            let Ok(meta) = entry.metadata() else { continue };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            out.push(format!(
                "{dir}/{} {} bytes mtime {mtime}",
                entry.file_name().to_string_lossy(),
                meta.len()
            ));
        }
    }
    out
}

/// `name-<16 hex>` into the name and the unit hash.
fn split_unit(entry: &str) -> Option<(&str, &str)> {
    let (name, hash) = entry.rsplit_once('-')?;
    is_hash(hash).then_some((name, hash))
}

fn is_hash(text: &str) -> bool {
    text.len() == 16 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Copy a file or directory tree, keeping each file's mtime: cargo compares
/// a unit's outputs against its dependencies' by mtime.
fn copy_tree(src: &Path, dst: &Path) -> io::Result<u64> {
    let meta = fs::symlink_metadata(src)?;
    if meta.is_symlink() {
        return Err(io::Error::other(format!("{} is a symlink", src.display())));
    }
    if meta.is_dir() {
        fs::create_dir_all(dst)?;
        let mut bytes = 0;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            bytes += copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        return Ok(bytes);
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    // A read-only leftover from an earlier cut-short seed would refuse the copy.
    let _ = fs::remove_file(dst);
    let bytes = fs::copy(src, dst)?;
    fs::File::open(dst)?.set_modified(meta.modified()?)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    #[test]
    fn splits_unit_names() {
        assert_eq!(
            split_unit("serde_json-0123456789abcdef"),
            Some(("serde_json", "0123456789abcdef"))
        );
        assert_eq!(
            split_unit("aws-lc-sys-0123456789abcdef"),
            Some(("aws-lc-sys", "0123456789abcdef"))
        );
        assert_eq!(split_unit("build-script-build"), None);
        assert_eq!(split_unit("nohash"), None);
    }

    fn profile_dir(root: &Path, units: &[(&str, &[&str])]) {
        for (unit, files) in units {
            fs::create_dir_all(root.join(".fingerprint").join(unit)).unwrap();
            fs::write(root.join(".fingerprint").join(unit).join("lib"), unit).unwrap();
            for file in *files {
                let path = root.join(file);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, unit).unwrap();
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_unit_that_cannot_be_copied_is_skipped_without_its_fingerprint() {
        let root = std::env::temp_dir().join(format!("soothfast-seed-skip-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (head, base) = (root.join("head"), root.join("base"));
        profile_dir(
            &head,
            &[
                (
                    "good-0123456789abcdef",
                    &["deps/libgood-0123456789abcdef.rlib"],
                ),
                (
                    "bad-fedcba9876543210",
                    &["build/bad-fedcba9876543210/out/a"],
                ),
            ],
        );
        std::os::unix::fs::symlink("a", head.join("build/bad-fedcba9876543210/out/l")).unwrap();

        let seeded = seed(&head, &base, &BTreeSet::new()).unwrap();
        assert_eq!(seeded.units, 1);
        assert_eq!(seeded.skipped.len(), 1, "{:?}", seeded.skipped);
        assert!(base.join(".fingerprint/good-0123456789abcdef").exists());
        assert!(!base.join(".fingerprint/bad-fedcba9876543210").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_per_package_layout_seeds_dependencies_but_not_members() {
        let root = std::env::temp_dir().join(format!("soothfast-seed-nest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (head, base) = (root.join("head"), root.join("base"));
        for unit in ["serde/0123456789abcdef", "app/fedcba9876543210"] {
            for sub in ["out/x", "fingerprint/lib"] {
                let path = head.join("build").join(unit).join(sub);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, unit).unwrap();
            }
        }
        let members = BTreeSet::from(["app".to_string()]);
        let seeded = seed(&head, &base, &members).unwrap();
        assert_eq!((seeded.units, seeded.skipped.len()), (1, 0), "{seeded:?}");
        assert!(
            base.join("build/serde/0123456789abcdef/fingerprint/lib")
                .exists()
        );
        assert!(base.join("build/serde/0123456789abcdef/out/x").exists());
        assert!(!base.join("build/app").exists());
        assert_eq!(seed(&head, &base, &members).unwrap().units, 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_package_directory_is_skipped_not_fatal() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("soothfast-seed-deny-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (head, base) = (root.join("head"), root.join("base"));
        for unit in ["locked/0123456789abcdef", "open/fedcba9876543210"] {
            let path = head.join("build").join(unit).join("out/x");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, unit).unwrap();
        }
        let locked = head.join("build/locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let seeded = seed(&head, &base, &BTreeSet::new()).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let readable = fs::read_dir(&locked).is_ok();
        assert_eq!(seeded.units, 1);
        assert_eq!(seeded.skipped.len(), 1, "{seeded:?}");
        assert!(readable);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_files_seed_and_reseed() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("soothfast-seed-ro-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (head, base) = (root.join("head"), root.join("base"));
        let file = "build/ro-0123456789abcdef/out/gen.rs";
        profile_dir(&head, &[("ro-0123456789abcdef", &[file])]);
        fs::set_permissions(head.join(file), fs::Permissions::from_mode(0o444)).unwrap();

        assert_eq!(seed(&head, &base, &BTreeSet::new()).unwrap().units, 1);
        fs::remove_dir_all(base.join(".fingerprint")).unwrap();
        let again = seed(&head, &base, &BTreeSet::new()).unwrap();
        assert_eq!((again.units, again.skipped.len()), (1, 0), "{again:?}");
        let _ = fs::remove_dir_all(&root);
    }

    /// The version this workspace's `Cargo.lock` holds for `name`, which an
    /// `--offline` build can resolve because the workspace build fetched it.
    fn locked(name: &str) -> String {
        let lock =
            fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../Cargo.lock")).unwrap();
        let entry = format!("name = \"{name}\"\nversion = \"");
        let start = lock.find(&entry).unwrap() + entry.len();
        lock[start..].split('"').next().unwrap().to_string()
    }

    fn write_workspace(root: &Path, deps: &str, body: &str) {
        let files = [
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"m\"]\nresolver = \"2\"\n".to_string(),
            ),
            (
                "m/Cargo.toml",
                format!(
                    "[package]\nname = \"m\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}"
                ),
            ),
            ("m/src/lib.rs", body.to_string()),
        ];
        for (path, text) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }

    /// `cargo build --release` in `root`; each unit's (name, fresh).
    fn build(root: &Path, target: &Path) -> Vec<(String, bool)> {
        let out = Command::new("cargo")
            .args(["build", "--release", "--offline", "--message-format=json"])
            .current_dir(root)
            .env("CARGO_TARGET_DIR", target)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        crate::invoke::cargo_messages(&out.stdout)
            .filter(|m| m["reason"] == "compiler-artifact")
            .map(|m| {
                let name = m["target"]["name"].as_str().unwrap_or_default().to_string();
                (name, m["fresh"].as_bool().unwrap_or(false))
            })
            .collect()
    }

    fn tree(dir: &Path, out: &mut BTreeMap<PathBuf, (u64, std::time::SystemTime)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            if meta.is_dir() {
                tree(&entry.path(), out);
            } else {
                let bytes = fs::read(entry.path()).unwrap_or_default();
                let hash = soothfast_registry::fnv1a(&bytes);
                out.insert(entry.path(), (hash ^ meta.len(), meta.modified().unwrap()));
            }
        }
    }

    #[test]
    fn base_build_reuses_seeded_dependencies_and_leaves_head_alone() {
        let root = std::env::temp_dir().join(format!("soothfast-seed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (head, base) = (root.join("head"), root.join("base"));
        let (head_t, base_t) = (root.join("head-target"), root.join("base-target"));
        let pin = |name: &str| format!("{name} = \"={}\"\n", locked(name));
        let shared = format!("{}{}", pin("itoa"), pin("libc"));
        write_workspace(&head, &shared, "pub fn f() -> u8 { 1 }\n");
        write_workspace(
            &base,
            &format!("{shared}{}", pin("memchr")),
            "pub fn f() -> u8 { 2 }\n",
        );

        build(&head, &head_t);
        let mut before = BTreeMap::new();
        tree(&head_t, &mut before);

        let members = BTreeSet::from(["m".to_string()]);
        let seeded = seed(&head_t.join("release"), &base_t.join("release"), &members).unwrap();
        assert!(seeded.units >= 2, "{seeded:?}");
        let units = build(&base, &base_t);
        let fresh = |name: &str| units.iter().find(|(n, _)| n == name).map(|(_, f)| *f);
        assert_eq!(fresh("itoa"), Some(true), "{units:?}");
        assert_eq!(fresh("libc"), Some(true), "{units:?}");
        assert_eq!(fresh("memchr"), Some(false), "only base depends on it");
        assert_eq!(fresh("m"), Some(false), "a member is never seeded");

        let mut after = BTreeMap::new();
        tree(&head_t, &mut after);
        assert_eq!(before, after, "the base build changed HEAD's target dir");
        let _ = fs::remove_dir_all(&root);
    }
}
