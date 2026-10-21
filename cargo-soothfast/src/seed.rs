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
//! Files are copied, never hardlinked: through a shared inode, any in-place
//! write by the base build would change HEAD's artifacts between
//! measurements. `fs::copy` reflinks where the filesystem can. The
//! copies do not point back into HEAD's dir: cargo rewrites a build script's
//! recorded `OUT_DIR` to wherever its output now lives.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;
use std::time::Instant;

use crate::{invoke, workspace};

/// What one [`seed`] call copied.
#[derive(Debug, Default, PartialEq)]
pub struct Seeded {
    pub units: usize,
    pub bytes: u64,
}

/// Copy the units in profile dir `head` that belong to no package in
/// `members` into profile dir `base`, skipping units `base` already has.
pub fn seed(head: &Path, base: &Path, members: &BTreeSet<String>) -> io::Result<Seeded> {
    let mut seeded = Seeded::default();
    let Ok(fingerprints) = fs::read_dir(head.join(".fingerprint")) else {
        return Ok(seeded);
    };
    let mut wanted = BTreeMap::new();
    for entry in fingerprints {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some((pkg, hash)) = split_unit(&name)
            && !members.contains(pkg)
            && fs::symlink_metadata(base.join(".fingerprint").join(&name)).is_err()
        {
            wanted.insert(hash.to_string(), name);
        }
    }
    for dir in ["build", "deps"] {
        let Ok(entries) = fs::read_dir(head.join(dir)) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name.split('.').next().unwrap_or(&name);
            if split_unit(stem).is_some_and(|(_, hash)| wanted.contains_key(hash)) {
                seeded.bytes += copy_tree(&entry.path(), &base.join(dir).join(&name))?;
            }
        }
    }
    // A unit's fingerprint goes last: an interrupted seed leaves the unit
    // without one, so cargo rebuilds it instead of trusting a partial copy.
    for name in wanted.values() {
        let fingerprint = Path::new(".fingerprint").join(name);
        seeded.bytes += copy_tree(&head.join(&fingerprint), &base.join(&fingerprint))?;
        seeded.units += 1;
    }
    Ok(seeded)
}

/// Seed the merge-base worktree's bench profile dir from HEAD's, which must
/// already be built. A failure only leaves the base build cold.
pub fn seed_worktree(wt: &Path, wt_target: &Path) {
    let start = Instant::now();
    let (Ok(head_target), Ok(head_members), Ok(base_members)) = (
        invoke::target_dir(None),
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
        Ok(s) if s.units > 0 => println!(
            "gate: seeded {} dependency units ({:.1} MiB) from HEAD's target dir in {:.1}s",
            s.units,
            s.bytes as f64 / (1024.0 * 1024.0),
            start.elapsed().as_secs_f64()
        ),
        Ok(_) => {}
        Err(e) => println!(
            "gate: could not seed the merge-base target dir ({e}); its dependencies build cold"
        ),
    }
}

/// `name-<16 hex>` into the name and the unit hash.
fn split_unit(entry: &str) -> Option<(&str, &str)> {
    let (name, hash) = entry.rsplit_once('-')?;
    (hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some((name, hash))
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
    let bytes = fs::copy(src, dst)?;
    fs::File::options()
        .write(true)
        .open(dst)?
        .set_modified(meta.modified()?)?;
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
            .args(["build", "--release", "--message-format=json"])
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
        let shared = "itoa = \"=1.0.18\"\nlibc = \"=0.2.190\"\n";
        write_workspace(&head, shared, "pub fn f() -> u8 { 1 }\n");
        write_workspace(
            &base,
            &format!("{shared}memchr = \"=2.8.3\"\n"),
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
