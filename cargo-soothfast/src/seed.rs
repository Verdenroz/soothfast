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
//! ([`crate::unitgraph`]). A unit the merge-base holds counts as HEAD's copy
//! only while its output files match HEAD's in name, size and mtime, which a
//! copy keeps and any rebuild in place breaks.
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
use std::time::{Instant, SystemTime};

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
    let _ = fs::remove_file(base.join(STALE_RECORD));
    let mut seeded = Seeded::default();
    for layout in [Layout::Flat, Layout::Nested] {
        seed_layout(layout, head, base, members, &mut seeded)?;
    }
    Ok(seeded)
}

/// A list of seeded units no build reads; outputs say which units are copies.
const STALE_RECORD: &str = ".soothfast-seeded";

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
    seeded: &mut Seeded,
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
    let (head_graph, base_graph) = (graph(&head_dirs), graph(&base_dirs));
    let touched = unitgraph::touching(&head_graph, &base_graph, &candidates);
    let touched: Vec<&str> = touched.iter().map(String::as_str).collect();
    let mut listed = touched.clone();
    listed.extend(candidates.iter().map(String::as_str));
    let head_files = listing(layout, head, &listed, &mut seeded.unreadable);
    let base_files = listing(layout, base, &touched, &mut seeded.unreadable);
    let copies = copies(&head_files, &base_files, &touched);
    let blocked = unitgraph::blocked(&head_graph, &base_graph, &copies, &candidates);
    for name in candidates {
        if let Some(why) = blocked.get(&name) {
            seeded.skipped.push(format!("{name}: {why}"));
            continue;
        }
        let Some(files) = head_files.complete_files(&name) else {
            seeded
                .skipped
                .push(format!("{name}: not every file of it could be listed"));
            continue;
        };
        let fingerprint = match layout {
            Layout::Flat => Path::new(".fingerprint").join(&name),
            Layout::Nested => Path::new("build").join(&name).join("fingerprint"),
        };
        match copy_unit(head, base, files, &fingerprint) {
            Ok(bytes) => {
                seeded.units += 1;
                seeded.bytes += bytes;
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
            for entry in entries {
                match entry {
                    Ok(entry) => {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if split_unit(&name).is_some() {
                            out.insert(name, entry.path());
                        }
                    }
                    Err(e) => unreadable.push(format!(".fingerprint: {e}")),
                }
            }
        }
        Layout::Nested => {
            let Ok(pkgs) = fs::read_dir(profile.join("build")) else {
                return out;
            };
            for pkg in pkgs {
                let pkg = match pkg {
                    Ok(pkg) => pkg,
                    Err(e) => {
                        unreadable.push(format!("build: {e}"));
                        continue;
                    }
                };
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
                for unit in units {
                    let unit = match unit {
                        Ok(unit) => unit,
                        Err(e) => {
                            unreadable.push(format!("{name}: {e}"));
                            continue;
                        }
                    };
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

/// Each output file of a unit, by its path under the profile dir, with its
/// size and mtime.
type Outputs = BTreeSet<(PathBuf, u64, SystemTime)>;

/// The output files of some units of one profile dir, listed once.
struct Listing {
    /// `None` for a unit with an entry that could not be read.
    units: BTreeMap<String, Option<Outputs>>,
    /// False when an entry of the shared `build/` or `deps/` could not be
    /// read, which may hide a file of any flat unit.
    complete: bool,
}

impl Listing {
    fn complete_files(&self, name: &str) -> Option<&Outputs> {
        self.units
            .get(name)
            .and_then(Option::as_ref)
            .filter(|_| self.complete)
    }
}

/// The units of `names` whose outputs in `base` match HEAD's in name, size
/// and mtime: what a seed's copy leaves and any rebuild in place breaks.
fn copies(head: &Listing, base: &Listing, names: &[&str]) -> BTreeSet<String> {
    names
        .iter()
        .filter(|name| {
            head.complete_files(name)
                .is_some_and(|o| !o.is_empty() && base.complete_files(name) == Some(o))
        })
        .map(|name| name.to_string())
        .collect()
}

/// The outputs of each of `names` in `profile`: a flat unit's `build/` and
/// `deps/` entries, a nested unit's directory without its fingerprint.
fn listing(
    layout: Layout,
    profile: &Path,
    names: &[&str],
    unreadable: &mut Vec<String>,
) -> Listing {
    let mut units: BTreeMap<String, Option<Outputs>> = names
        .iter()
        .map(|name| (name.to_string(), Some(Outputs::new())))
        .collect();
    let mut complete = true;
    match layout {
        Layout::Flat => {
            let by_hash: BTreeMap<&str, &str> = names
                .iter()
                .filter_map(|name| split_unit(name).map(|(_, hash)| (hash, *name)))
                .collect();
            for dir in ["build", "deps"] {
                let Ok(entries) = fs::read_dir(profile.join(dir)) else {
                    continue;
                };
                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(e) => {
                            unreadable.push(format!("{dir}: {e}"));
                            complete = false;
                            continue;
                        }
                    };
                    let file = entry.file_name().to_string_lossy().into_owned();
                    let stem = file.split('.').next().unwrap_or(&file);
                    if let Some((_, hash)) = split_unit(stem)
                        && let Some(name) = by_hash.get(hash)
                        && let Some(files) = units.get_mut(*name)
                    {
                        walk(profile, &Path::new(dir).join(&file), files, unreadable);
                    }
                }
            }
        }
        Layout::Nested => {
            for (name, files) in &mut units {
                let dir = Path::new("build").join(name);
                let entries = match fs::read_dir(profile.join(&dir)) {
                    Ok(entries) => entries,
                    Err(e) => {
                        unreadable.push(format!("{name}: {e}"));
                        *files = None;
                        continue;
                    }
                };
                for entry in entries {
                    match entry {
                        Ok(e) if e.file_name() == "fingerprint" => {}
                        Ok(e) => walk(profile, &dir.join(e.file_name()), files, unreadable),
                        Err(e) => {
                            unreadable.push(format!("{name}: {e}"));
                            *files = None;
                        }
                    }
                }
            }
        }
    }
    Listing { units, complete }
}

/// Add the files under `rel` to `files`, or set it to `None` on an entry that
/// cannot be read or a symlink, which a copy would not reproduce.
fn walk(profile: &Path, rel: &Path, files: &mut Option<Outputs>, unreadable: &mut Vec<String>) {
    let Some(list) = files.as_mut() else {
        return;
    };
    let meta = match fs::symlink_metadata(profile.join(rel)) {
        Ok(meta) if !meta.is_symlink() => meta,
        Ok(_) => {
            unreadable.push(format!("{} is a symlink", rel.display()));
            *files = None;
            return;
        }
        Err(e) => {
            unreadable.push(format!("{}: {e}", rel.display()));
            *files = None;
            return;
        }
    };
    if !meta.is_dir() {
        match meta.modified() {
            Ok(mtime) => {
                list.insert((rel.to_path_buf(), meta.len(), mtime));
            }
            Err(_) => *files = None,
        }
        return;
    }
    let entries: Vec<_> = match fs::read_dir(profile.join(rel)) {
        Ok(entries) => entries.collect(),
        Err(e) => {
            unreadable.push(format!("{}: {e}", rel.display()));
            *files = None;
            return;
        }
    };
    for entry in entries {
        match entry {
            Ok(entry) => walk(profile, &rel.join(entry.file_name()), files, unreadable),
            Err(e) => {
                unreadable.push(format!("{}: {e}", rel.display()));
                *files = None;
            }
        }
    }
}

fn graph(dirs: &BTreeMap<String, PathBuf>) -> BTreeMap<String, Unit> {
    dirs.iter()
        .map(|(name, dir)| (name.clone(), unitgraph::read(dir)))
        .collect()
}

/// Copy a unit's listed `files`, then its `fingerprint`: a unit whose copy
/// fails or is cut short has none, so cargo rebuilds it instead of trusting
/// a partial copy.
fn copy_unit(head: &Path, base: &Path, files: &Outputs, fingerprint: &Path) -> io::Result<u64> {
    let mut bytes = 0;
    for (rel, _, _) in files {
        bytes += copy_tree(&head.join(rel), &base.join(rel))?;
    }
    if fs::symlink_metadata(head.join(fingerprint)).is_err() {
        return Ok(bytes);
    }
    Ok(bytes + copy_tree(&head.join(fingerprint), &base.join(fingerprint))?)
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

/// How many seeded units [`report`] names before it only counts.
const REPORTED_UNITS: usize = 20;

/// After a failed merge-base build, name the units of its target dir that
/// are still HEAD's copies.
pub fn report(wt_target: &Path) {
    let Some(head_target) = wt_target.parent() else {
        return;
    };
    let (head, base) = (head_target.join("release"), wt_target.join("release"));
    let mut units = Vec::new();
    for layout in [Layout::Flat, Layout::Nested] {
        let head_dirs = unit_dirs(layout, &head, &mut Vec::new());
        let shared: Vec<&str> = unit_dirs(layout, &base, &mut Vec::new())
            .into_keys()
            .filter_map(|name| head_dirs.get_key_value(&name).map(|(n, _)| n.as_str()))
            .collect();
        let (head_files, base_files) = (
            listing(layout, &head, &shared, &mut Vec::new()),
            listing(layout, &base, &shared, &mut Vec::new()),
        );
        units.extend(copies(&head_files, &base_files, &shared));
    }
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
