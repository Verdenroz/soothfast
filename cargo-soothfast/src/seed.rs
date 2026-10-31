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
//! A unit is left to the base build when it sits on a unit the merge-base
//! already built itself: one name can cover two builds rustc tells apart
//! ([`crate::unitgraph`]). Each copy is recorded in the profile dir, so a
//! later seed knows a unit it copied from one the merge-base built.
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

use crate::unitgraph::{self, Unit};
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
    /// Package directories of HEAD that could not be listed.
    pub unreadable: Vec<String>,
}

/// Copy the units in profile dir `head` that belong to no package in
/// `members` into profile dir `base`, skipping units `base` already has.
pub fn seed(head: &Path, base: &Path, members: &BTreeSet<String>) -> io::Result<Seeded> {
    let record = read_record(base);
    let mut seeded = Seeded::default();
    let mut copied = Vec::new();
    for layout in [Layout::Flat, Layout::Nested] {
        seed_layout(
            layout,
            head,
            base,
            members,
            &record,
            &mut seeded,
            &mut copied,
        )?;
    }
    append_record(base, &copied);
    Ok(seeded)
}

#[derive(Clone, Copy)]
enum Layout {
    /// `.fingerprint/`, `build/` and `deps/` side by side.
    Flat,
    /// One `build/<pkg>/<hash>/` directory per unit.
    Nested,
}

fn seed_layout(
    layout: Layout,
    head: &Path,
    base: &Path,
    members: &BTreeSet<String>,
    record: &BTreeMap<String, String>,
    seeded: &mut Seeded,
    copied: &mut Vec<(String, String)>,
) -> io::Result<()> {
    let head_dirs = unit_dirs(layout, head, &mut seeded.unreadable);
    let base_dirs = unit_dirs(layout, base, &mut Vec::new());
    let candidates: Vec<String> = head_dirs
        .keys()
        .filter(|name| !base_dirs.contains_key(*name) && !members.contains(package(layout, name)))
        .cloned()
        .collect();
    if candidates.is_empty() {
        return Ok(());
    }
    let head_graph = graph(&head_dirs);
    let blocked = unitgraph::blocked(&head_graph, &graph(&base_dirs), record, &candidates);
    seeded
        .skipped
        .extend(blocked.iter().map(|(name, why)| format!("{name}: {why}")));
    let copy: Vec<String> = candidates
        .into_iter()
        .filter(|name| !blocked.contains_key(name))
        .collect();
    let results = match layout {
        Layout::Flat => copy_flat(head, base, &copy)?,
        Layout::Nested => copy
            .iter()
            .map(|unit| {
                let rel = Path::new("build").join(unit);
                (unit.clone(), copy_unit(&head.join(&rel), &base.join(&rel)))
            })
            .collect(),
    };
    for (name, result) in results {
        match result {
            Ok(bytes) => {
                seeded.units += 1;
                seeded.bytes += bytes;
                copied.push((name.clone(), head_graph[&name].key()));
                seeded.names.push(name);
            }
            Err(e) => seeded.skipped.push(format!("{name}: {e}")),
        }
    }
    Ok(())
}

/// The package a unit belongs to.
fn package(layout: Layout, name: &str) -> &str {
    match layout {
        Layout::Flat => split_unit(name).map_or(name, |(pkg, _)| pkg),
        Layout::Nested => name.split('/').next().unwrap_or(name),
    }
}

/// Each unit of `profile` in `layout`, by name, with its fingerprint dir.
fn unit_dirs(
    layout: Layout,
    profile: &Path,
    unreadable: &mut Vec<String>,
) -> BTreeMap<String, PathBuf> {
    let mut out = BTreeMap::new();
    match layout {
        Layout::Flat => {
            let Ok(entries) = fs::read_dir(profile.join(".fingerprint")) else {
                return out;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if split_unit(&name).is_some() {
                    out.insert(name, entry.path());
                }
            }
        }
        Layout::Nested => {
            let Ok(pkgs) = fs::read_dir(profile.join("build")) else {
                return out;
            };
            for pkg in pkgs.flatten() {
                let name = pkg.file_name().to_string_lossy().into_owned();
                if split_unit(&name).is_some() || !pkg.path().is_dir() {
                    continue;
                }
                let units = match fs::read_dir(pkg.path()) {
                    Ok(units) => units,
                    Err(e) => {
                        unreadable.push(format!("{name}: {e}"));
                        continue;
                    }
                };
                for unit in units.flatten() {
                    let hash = unit.file_name().to_string_lossy().into_owned();
                    if is_hash(&hash) {
                        out.insert(format!("{name}/{hash}"), unit.path().join("fingerprint"));
                    }
                }
            }
        }
    }
    out
}

fn graph(dirs: &BTreeMap<String, PathBuf>) -> BTreeMap<String, Unit> {
    dirs.iter()
        .map(|(name, dir)| (name.clone(), unitgraph::read(dir)))
        .collect()
}

/// Copy flat units: each one's `build/` and `deps/` files, then its
/// fingerprint.
fn copy_flat(
    head: &Path,
    base: &Path,
    names: &[String],
) -> io::Result<Vec<(String, io::Result<u64>)>> {
    let mut units: BTreeMap<&str, (&str, Vec<PathBuf>)> = names
        .iter()
        .filter_map(|name| split_unit(name).map(|(_, hash)| (hash, (name.as_str(), Vec::new()))))
        .collect();
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
    Ok(units
        .into_values()
        .map(|(fingerprint, mut files)| {
            files.push(Path::new(".fingerprint").join(fingerprint));
            let copied = files.iter().try_fold(0, |bytes, rel| {
                Ok::<_, io::Error>(bytes + copy_tree(&head.join(rel), &base.join(rel))?)
            });
            (fingerprint.to_string(), copied)
        })
        .collect())
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
            if let Some(first) = s.unreadable.first() {
                println!(
                    "gate: could not list {} package dirs, first {first}",
                    s.unreadable.len()
                );
            }
        }
        Err(e) => println!(
            "gate: could not seed the merge-base target dir ({e}); its dependencies build cold"
        ),
    }
}

/// The record in a profile dir of every unit a seed copied into it, each
/// with the key of HEAD's copy, in runs that each start with a `run` line.
const SEEDED: &str = ".soothfast-seeded";

/// How many seeded units [`report`] names before it only counts.
const REPORTED_UNITS: usize = 20;

/// The units the record holds, at the key each was last seeded under.
fn read_record(profile: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(profile.join(SEEDED))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(' '))
        .filter(|(name, _)| *name != "run")
        .map(|(name, key)| (name.to_string(), key.to_string()))
        .collect()
}

fn append_record(profile: &Path, copied: &[(String, String)]) {
    use std::io::Write;
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut text = format!("run {started}\n");
    for (name, key) in copied {
        text.push_str(&format!("{name} {key}\n"));
    }
    let _ = fs::create_dir_all(profile);
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(profile.join(SEEDED))
        .and_then(|mut f| f.write_all(text.as_bytes()));
}

/// The units the latest seed copied into `profile`.
fn latest_run(profile: &Path) -> Vec<String> {
    let text = fs::read_to_string(profile.join(SEEDED)).unwrap_or_default();
    let mut units = Vec::new();
    for line in text.lines() {
        match line.split_once(' ') {
            Some(("run", _)) => units.clear(),
            Some((name, _)) => units.push(name.to_string()),
            None => {}
        }
    }
    units
}

/// After a failed merge-base build, name what this run's seed copied into
/// its target dir.
pub fn report(wt_target: &Path) {
    let units = latest_run(&wt_target.join("release"));
    for unit in units.iter().take(REPORTED_UNITS) {
        eprintln!("gate: seeded unit {unit}");
    }
    if units.len() > REPORTED_UNITS {
        eprintln!(
            "gate: and {} more seeded units",
            units.len() - REPORTED_UNITS
        );
    }
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
mod tests;
