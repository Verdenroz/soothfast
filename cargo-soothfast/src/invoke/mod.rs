//! Subprocess plumbing: run the user's soothfast bench binary via `cargo bench`,
//! parse its JSONL, and read/write baseline files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

mod args;
mod baseline;
mod bench;
mod git;
mod hostenv;
mod root;
mod run;
mod rustdoc;

pub use args::{CommonArgs, Shard};
#[cfg(test)]
pub use baseline::baseline_path;
pub use baseline::{
    SaveScope, id_pkg, load_baseline, load_baseline_in, save_baseline, save_baseline_in,
    write_baseline_in,
};
pub(crate) use bench::{Build, bench_artifact, bench_build_command, bench_toolchain};
pub use bench::{
    bench_executable, build_link_map, cargo_messages, listed_ids, prebuild_benches, run_bench,
    run_bench_dir, run_bench_in, run_bench_raw, run_discovery, run_discovery_in,
};
pub use git::{git, git_in};
#[cfg(test)]
pub use hostenv::Guest;
pub use hostenv::{HostEnv, host_env, list_items};
pub use root::{
    PkgMeta, buildcost_base_target_dir, buildcost_target_dir, pkg_dir, pkg_meta, project_root,
    target_dir, workspace_root, worktree_target_dir,
};
use run::saved_items;
pub use run::{
    AssertionOutcome, ItemMetrics, Reused, Run, collect, pair, run_from_items_value,
    run_to_items_value,
};
pub(crate) use rustdoc::{RUSTDOC_FORMAT_VERSION, crate_dirs};
pub use rustdoc::{Visibility, rustdoc_holds, rustdoc_json_in, rustdoc_key};

/// Run `f`, printing its wall time to stderr when `SOOTHFAST_TIMINGS` is set.
pub fn timed<T>(label: &str, f: impl FnOnce() -> T) -> T {
    if std::env::var_os("SOOTHFAST_TIMINGS").is_none() {
        return f();
    }
    let start = Instant::now();
    let out = f();
    eprintln!(
        "soothfast-timing: {label} {:.1}s",
        start.elapsed().as_secs_f64()
    );
    out
}
/// Check out the merge-base of HEAD and `refname` under
/// `.soothfast/worktrees/<sha>` and run `f` against it.
///
/// The checkout is kept between runs. Re-creating it would rewrite every
/// file's mtime, and cargo's freshness for path dependencies is mtime-based,
/// so the shared worktree target dir would rebuild every workspace member.
pub fn with_merge_base_worktree<T>(
    refname: &str,
    f: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    let sha = git(&["merge-base", "HEAD", refname]).map_err(|e| e.to_string())?;
    let root = workspace_root().map_err(|e| e.to_string())?;
    let dir = root.join(".soothfast").join("worktrees");
    let wt = dir.join(&sha);
    let wt_str = wt
        .to_str()
        .ok_or_else(|| "worktree path is not UTF-8".to_string())?;

    forget_missing_worktree(&wt);
    discard_worktrees_except(&dir, &sha);

    if is_worktree_root(&wt) {
        git(&["-C", wt_str, "checkout", "--force", "--detach", &sha]).map_err(|e| e.to_string())?;
        git(&["-C", wt_str, "clean", "-fd"]).map_err(|e| e.to_string())?;
    } else {
        let _ = std::fs::remove_dir_all(&wt);
        git(&["worktree", "add", "--detach", wt_str, &sha]).map_err(|e| e.to_string())?;
    }
    verify_worktree(wt_str, &sha)?;
    f(&wt)
}

/// Whether `wt` is itself the root of a linked worktree. Testing for a
/// reachable git dir would accept any directory inside the repository and
/// send a later `checkout` at the main checkout instead.
fn is_worktree_root(wt: &Path) -> bool {
    if !wt.join(".git").is_file() {
        return false;
    }
    let Some(wt_str) = wt.to_str() else {
        return false;
    };
    git(&["-C", wt_str, "rev-parse", "--show-toplevel"])
        .is_ok_and(|top| same_path(Path::new(&top), wt))
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The reference side is only worth measuring if the tree it measures is the
/// merge-base, so a reused checkout has to prove it still is.
fn verify_worktree(wt: &str, sha: &str) -> Result<(), String> {
    let head = git(&["-C", wt, "rev-parse", "HEAD"]).map_err(|e| e.to_string())?;
    if head != sha {
        return Err(format!("worktree {wt} is at {head}, not merge-base {sha}"));
    }
    match git(&["-C", wt, "status", "--porcelain"]) {
        Ok(out) if out.is_empty() => Ok(()),
        Ok(_) => Err(format!("worktree {wt} has local changes; delete it")),
        Err(e) => Err(e.to_string()),
    }
}

/// Drop checkouts of every other merge-base, so a rebased branch does not
/// leave one behind per base it has had.
fn discard_worktrees_except(dir: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name() == std::ffi::OsStr::new(keep) {
            continue;
        }
        if let Some(path) = entry.path().to_str() {
            let _ = git(&["worktree", "remove", "--force", path]);
        }
        let _ = std::fs::remove_dir_all(entry.path());
        forget_missing_worktree(&entry.path());
    }
}

/// Drop the registration of `wt` when its directory is gone. A deleted
/// worktree stays registered and `add` refuses the path. Never `git worktree
/// prune`: that drops every other worktree whose directory this process
/// cannot see, such as one on an unmounted drive or outside a container.
pub fn forget_missing_worktree(wt: &Path) {
    if let Ok(root) = workspace_root() {
        forget_missing_worktree_in(&root, wt);
    }
}

fn forget_missing_worktree_in(repo: &Path, wt: &Path) {
    if wt.exists() {
        return;
    }
    if let Some(path) = wt.to_str() {
        let _ = git_in(repo, &["worktree", "remove", "--force", "--force", path]);
    }
}

/// One `[[package]]` entry from a Cargo.lock.
pub(crate) struct LockPkg {
    pub(crate) name: String,
    pub(crate) version: String,
    /// Registry entries carry a `source` line; path dependencies don't and
    /// cannot be re-pinned.
    pub(crate) registry: bool,
}

pub(crate) fn lock_packages(lock: &str) -> Vec<LockPkg> {
    lock.split("[[package]]")
        .skip(1)
        .filter_map(|block| {
            let field = |key: &str| {
                block
                    .lines()
                    .find_map(|l| l.strip_prefix(key))
                    .map(|v| v.trim().trim_matches('"').to_string())
            };
            Some(LockPkg {
                name: field("name = ")?,
                version: field("version = ")?,
                registry: block.lines().any(|l| l.starts_with("source = ")),
            })
        })
        .collect()
}

/// Soothfast-family registry entries whose worktree version differs from
/// HEAD's: (name, worktree version, HEAD version).
fn harness_mismatches(head_lock: &str, wt_lock: &str) -> Vec<(String, String, String)> {
    let head: BTreeMap<String, LockPkg> = lock_packages(head_lock)
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect();
    lock_packages(wt_lock)
        .into_iter()
        .filter(|p| p.registry && (p.name == "soothfast" || p.name.starts_with("soothfast-")))
        .filter_map(|p| {
            let h = head.get(&p.name).filter(|h| h.registry)?;
            (h.version != p.version).then(|| (p.name, p.version, h.version.clone()))
        })
        .collect()
}

/// HEAD's locked soothfast versions, as `name@version` pairs. The reference
/// side is pinned to these before it is built, so they decide what a
/// reference measurement means.
pub fn harness_versions() -> String {
    let Ok(root) = workspace_root() else {
        return String::new();
    };
    let Ok(lock) = std::fs::read_to_string(root.join("Cargo.lock")) else {
        return String::new();
    };
    let mut out: Vec<String> = lock_packages(&lock)
        .into_iter()
        .filter(|p| p.name == "soothfast" || p.name.starts_with("soothfast-"))
        .map(|p| format!("{}@{}", p.name, p.version))
        .collect();
    out.sort();
    out.join(",")
}

/// Whether the working tree has no uncommitted changes, so HEAD's SHA names
/// what was actually measured.
pub fn tree_is_clean() -> bool {
    git(&["status", "--porcelain"]).is_ok_and(|s| s.trim().is_empty())
}

/// Whether the reference side ended up embedding HEAD's measurement harness.
/// A reference built against another harness measures a different protocol,
/// so the outcome travels with the run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarnessSync {
    /// Crates cargo would not move, each spelled `name 0.2.0 -> 0.3.1`.
    unpinned: Vec<String>,
    /// The callgrind methods of the reference and of HEAD, when they differ
    /// or the reference's is unknown.
    method: Option<(String, String)>,
}

impl HarnessSync {
    /// Rebuild an outcome from the crate labels a stored run kept.
    pub fn from_unpinned(unpinned: Vec<String>) -> Self {
        HarnessSync {
            unpinned,
            method: None,
        }
    }

    /// The crates left at a version of their own, empty when the pin held.
    pub fn unpinned(&self) -> &[String] {
        &self.unpinned
    }

    /// This outcome with the callgrind methods of the two sides.
    pub fn with_method(self, base: &str, head: &str) -> Self {
        HarnessSync {
            method: Some((base.into(), head.into())),
            ..self
        }
    }

    /// The stored form of [`HarnessSync::with_method`]: `[base, head]`.
    pub fn method(&self) -> Option<[&str; 2]> {
        self.method.as_ref().map(|(b, h)| [b.as_str(), h.as_str()])
    }

    /// The method difference, spelled for a reader, when there is one.
    pub fn method_note(&self) -> Option<String> {
        self.method().map(|[base, head]| {
            format!("base measured with callgrind method {base}, head with {head}")
        })
    }

    /// Whether the two sides embed different measurement harnesses.
    pub fn is_mismatched(&self) -> bool {
        !self.unpinned.is_empty() || self.method.is_some()
    }
}

fn sync_outcome(mismatches: &[(String, String, String)]) -> HarnessSync {
    HarnessSync::from_unpinned(
        mismatches
            .iter()
            .map(|(name, from, to)| format!("{name} {from} -> {to}"))
            .collect(),
    )
}

/// Pin the merge-base worktree's soothfast crates to HEAD's locked versions
/// so the reference bench binary embeds the same measurement harness. A pin
/// the worktree's own requirement excludes is retried after rewriting that
/// requirement; anything still unpinned comes back as `Mismatched` for the
/// caller to put in front of the verdict.
pub fn sync_harness_versions(wt: &Path) -> Result<HarnessSync, String> {
    let head_path = workspace_root()
        .map_err(|e| e.to_string())?
        .join("Cargo.lock");
    let Ok(head) = std::fs::read_to_string(&head_path) else {
        return Ok(HarnessSync::default());
    };
    // One crate per pass, re-reading the lock: pinning the facade drags its
    // family along, and already-moved entries must not be warned about.
    for _ in 0..16 {
        let Ok(base) = std::fs::read_to_string(wt.join("Cargo.lock")) else {
            return Ok(HarnessSync::default());
        };
        let mismatches = harness_mismatches(&head, &base);
        let Some((name, from, to)) = mismatches.first().cloned() else {
            return Ok(HarnessSync::default());
        };
        println!(
            "gate: pinning {name} {from} -> {to} in the merge-base worktree (harness must match HEAD)"
        );
        let mut refused = pin_precise(wt, &name, &from, &to)?;
        // A caret-incompatible bump is refused by the merge-base's own
        // requirement. The worktree is disposable, so widen it and retry.
        if refused.as_deref().is_some_and(is_requirement_conflict)
            && relax_harness_requirements(wt, &mismatches)?
        {
            println!("gate: widened the merge-base's soothfast requirements to retry the pin");
            refused = pin_precise(wt, &name, &from, &to)?;
        }
        if let Some(stderr) = refused {
            eprintln!(
                "WARN: could not pin {name} ({}); the reference keeps its own harness — deltas may reflect the harness change itself",
                stderr.trim().lines().last().unwrap_or("no error output")
            );
            return Ok(sync_outcome(&mismatches));
        }
    }
    let remaining = std::fs::read_to_string(wt.join("Cargo.lock"))
        .map(|base| harness_mismatches(&head, &base))
        .unwrap_or_default();
    Ok(sync_outcome(&remaining))
}

/// Cargo's precise pin, run in the worktree. `Ok(None)` when it landed,
/// `Ok(Some(stderr))` when cargo refused it.
fn pin_precise(wt: &Path, name: &str, from: &str, to: &str) -> Result<Option<String>, String> {
    let out = Command::new("cargo")
        .args(["update", "-p", &format!("{name}@{from}"), "--precise", to])
        .current_dir(wt)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&out.stderr).into_owned()))
}

/// Whether cargo refused the pin because a requirement excludes the wanted
/// version, rather than because the index or network was unreachable.
fn is_requirement_conflict(stderr: &str) -> bool {
    stderr.contains("didn't match")
        || stderr.contains("does not match")
        || stderr.contains("failed to select a version")
}

/// Point the worktree's soothfast-family requirements at the versions HEAD
/// locks, so the pin above is no longer refused. Returns whether any
/// manifest changed.
fn relax_harness_requirements(
    wt: &Path,
    mismatches: &[(String, String, String)],
) -> Result<bool, String> {
    let wanted: BTreeMap<&str, &str> = mismatches
        .iter()
        .map(|(name, _, to)| (name.as_str(), to.as_str()))
        .collect();
    let mut changed = false;
    for path in manifests(wt) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(next) = retarget_requirements(&text, &wanted) else {
            continue;
        };
        std::fs::write(&path, next).map_err(|e| format!("{}: {e}", path.display()))?;
        changed = true;
    }
    Ok(changed)
}

/// Every `Cargo.toml` under `dir`. `target`/`.git` hold nothing the
/// resolver reads.
fn manifests(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            out.extend(manifests(&path));
        } else if name == "Cargo.toml" {
            out.push(path);
        }
    }
    out
}

/// Rewrite the `version` requirement of each dependency `wanted` names, in
/// one manifest's text. `None` when nothing matched. Only the requirement
/// string moves: re-specifying the dependency would drop `features`,
/// `optional` or `default-features` beside it.
fn retarget_requirements(text: &str, wanted: &BTreeMap<&str, &str>) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut table = String::new();
    let mut changed = false;
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches('\n').trim_end_matches('\r');
        let ending = &raw[line.len()..];
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            table = trimmed
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .to_string();
            out.push_str(raw);
            continue;
        }
        let rewritten = match dep_table_name(&table) {
            Some(name) => version_key(line)
                .zip(wanted.get(name))
                .and_then(|(at, to)| retarget_line(line, at, to)),
            None => dep_line(line, &table, wanted).and_then(|(at, to)| retarget_line(line, at, to)),
        };
        match rewritten {
            Some(next) => {
                out.push_str(&next);
                out.push_str(ending);
                changed = true;
            }
            None => out.push_str(raw),
        }
    }
    changed.then_some(out)
}

/// Where a `version = "..."` line's value starts.
fn version_key(line: &str) -> Option<usize> {
    let (key, rest) = line.split_once('=')?;
    (key.trim().trim_matches('"') == "version").then(|| line.len() - rest.len())
}

/// The dependency a `[…dependencies.NAME]` header names.
fn dep_table_name(table: &str) -> Option<&str> {
    let (parent, name) = table.rsplit_once('.')?;
    is_dep_table(parent).then_some(name)
}

fn is_dep_table(table: &str) -> bool {
    matches!(
        table.rsplit('.').next(),
        Some("dependencies" | "dev-dependencies" | "build-dependencies")
    )
}

/// Where a dependency line's requirement starts, and what it should become.
/// A path or git dependency is left alone: its version is not what the
/// resolver picks.
fn dep_line<'a>(
    line: &str,
    table: &str,
    wanted: &BTreeMap<&str, &'a str>,
) -> Option<(usize, &'a str)> {
    if !is_dep_table(table) {
        return None;
    }
    let (key, rest) = line.split_once('=')?;
    let to = wanted.get(key.trim().trim_matches('"'))?;
    let rest_at = line.len() - rest.len();
    let value = rest.trim_start();
    if value.starts_with('"') {
        return Some((rest_at, to));
    }
    if !value.starts_with('{') || value.contains("path =") || value.contains("git =") {
        return None;
    }
    let version_at = rest_at + rest.find("version")?;
    Some((version_at, to))
}

/// Replace the first quoted string at or after `from` with `to`, leaving
/// the rest of the line (other keys, a trailing comment) untouched.
fn retarget_line(line: &str, from: usize, to: &str) -> Option<String> {
    let open = from + line.get(from..)?.find('"')?;
    let close = open + 1 + line.get(open + 1..)?.find('"')?;
    (&line[open + 1..close] != to)
        .then(|| format!("{}\"{to}\"{}", &line[..open], &line[close + 1..]))
}

/// Copy an untracked `.cargo/config.toml` (or legacy `.cargo/config`) into
/// the worktree. `git worktree add` only checks out tracked content, so a
/// local file that shapes rustflags would otherwise silently vanish from
/// the reference build. A tracked config is left alone: it's the commit's
/// real content, which may differ on purpose.
pub fn sync_untracked_cargo_config(wt: &Path) -> Result<(), String> {
    let root = workspace_root().map_err(|e| e.to_string())?;
    for name in [".cargo/config.toml", ".cargo/config"] {
        let src = root.join(name);
        if !src.exists() || git(&["ls-files", "--error-unmatch", name]).is_ok() {
            continue;
        }
        let dst = wt.join(name);
        std::fs::create_dir_all(dst.parent().expect("config path has a parent"))
            .map_err(|e| e.to_string())?;
        std::fs::copy(&src, &dst).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{forget_missing_worktree_in, git_in, harness_mismatches};

    #[test]
    fn only_our_missing_worktree_is_forgotten() {
        let root = std::env::temp_dir().join(format!("soothfast-forget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let (ours, foreign) = (root.join("ours"), root.join("foreign"));
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| git_in(&repo, args).unwrap();
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ]);
        for wt in [&ours, &foreign] {
            git(&["worktree", "add", "-q", "--detach", wt.to_str().unwrap()]);
            std::fs::remove_dir_all(wt).unwrap();
        }
        forget_missing_worktree_in(&repo, &ours);
        let listing = git(&["worktree", "list", "--porcelain"]);
        assert!(!listing.contains("ours"), "{listing}");
        assert!(listing.contains("foreign"), "{listing}");
    }

    #[test]
    fn a_method_difference_mismatches_the_harness_without_a_pin() {
        let sync = super::HarnessSync::default().with_method("two-process", "one-process");
        assert!(sync.is_mismatched());
        assert!(sync.unpinned().is_empty());
        assert_eq!(
            sync.method_note().as_deref(),
            Some("base measured with callgrind method two-process, head with one-process")
        );
    }

    const HEAD: &str = r#"
version = 4

[[package]]
name = "serde"
version = "1.0.200"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "soothfast"
version = "0.1.7"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "soothfast-measure",
]

[[package]]
name = "soothfast-measure"
version = "0.1.7"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

    #[test]
    fn pins_family_crates_to_heads_versions() {
        let base = HEAD.replace("0.1.7", "0.1.5");
        assert_eq!(
            harness_mismatches(HEAD, &base),
            vec![
                ("soothfast".into(), "0.1.5".into(), "0.1.7".into()),
                ("soothfast-measure".into(), "0.1.5".into(), "0.1.7".into()),
            ]
        );
    }

    #[test]
    fn matching_locks_and_foreign_crates_need_nothing() {
        assert!(harness_mismatches(HEAD, HEAD).is_empty());
        // serde is measured code, not the harness — its bumps must stay gated.
        let base = HEAD.replace("1.0.200", "1.0.100");
        assert!(harness_mismatches(HEAD, &base).is_empty());
    }

    #[test]
    fn path_dependencies_are_skipped() {
        // Gating soothfast itself: family crates are path deps, no source line.
        let no_source = HEAD.replace(
            "source = \"registry+https://github.com/rust-lang/crates.io-index\"",
            "",
        );
        let base = no_source.replace("0.1.7", "0.1.5");
        assert!(harness_mismatches(&no_source, &base).is_empty());
    }

    #[test]
    fn a_caret_incompatible_bump_is_still_a_mismatch() {
        let base = HEAD.replace("0.1.7", "0.2.0");
        assert_eq!(
            harness_mismatches(HEAD, &base),
            vec![
                ("soothfast".into(), "0.2.0".into(), "0.1.7".into()),
                ("soothfast-measure".into(), "0.2.0".into(), "0.1.7".into()),
            ]
        );
    }

    fn wanted() -> BTreeMap<&'static str, &'static str> {
        BTreeMap::from([("soothfast", "0.3.1")])
    }

    #[test]
    fn a_requirement_conflict_is_told_apart_from_an_unreachable_index() {
        assert!(super::is_requirement_conflict(
            "error: failed to select a version for the requirement `soothfast = \"^0.2.0\"`\n\
             candidate versions found which didn't match: 0.3.1\n"
        ));
        assert!(!super::is_requirement_conflict(
            "error: failed to get `soothfast` as a dependency\n\
             Caused by: network failure seems to have happened\n"
        ));
    }

    #[test]
    fn every_requirement_shape_moves_to_heads_version() {
        let manifest = r#"[package]
name = "demo"

[dependencies]
soothfast = "0.2.0"
serde = "1.0.200"

[dev-dependencies]
soothfast = { version = "0.2.0", features = ["runner"], default-features = false }

[target.'cfg(unix)'.dependencies.soothfast]
version = "0.2.0"
optional = true
"#;
        let got = super::retarget_requirements(manifest, &wanted()).expect("rewritten");
        assert_eq!(got.matches("\"0.3.1\"").count(), 3);
        assert!(!got.contains("0.2.0"));
        assert!(got.contains("features = [\"runner\"], default-features = false"));
        assert!(got.contains("serde = \"1.0.200\""));
        assert!(got.contains("optional = true"));
    }

    #[test]
    fn a_manifest_with_nothing_to_move_is_left_alone() {
        let manifest = "[dependencies]\nsoothfast = \"0.3.1\"\nserde = \"1\"\n";
        assert!(super::retarget_requirements(manifest, &wanted()).is_none());
        // A path dep resolves from the tree, not from a requirement.
        let path_dep =
            "[dependencies]\nsoothfast = { path = \"../soothfast\", version = \"0.2.0\" }\n";
        assert!(super::retarget_requirements(path_dep, &wanted()).is_none());
        // `soothfast` outside a dependency table names something else.
        let other = "[features]\nsoothfast = \"0.2.0\"\n";
        assert!(super::retarget_requirements(other, &wanted()).is_none());
    }

    #[test]
    fn rewriting_keeps_the_rest_of_the_line() {
        let manifest = "[dependencies]\nsoothfast = \"0.2.0\" # the harness\n";
        let got = super::retarget_requirements(manifest, &wanted()).expect("rewritten");
        assert_eq!(got, "[dependencies]\nsoothfast = \"0.3.1\" # the harness\n");
    }

    #[test]
    fn an_unpinned_crate_survives_as_the_runs_verdict_context() {
        let matched = super::sync_outcome(&[]);
        assert!(!matched.is_mismatched());
        assert!(matched.unpinned().is_empty());
        let missed = super::sync_outcome(&[("soothfast".into(), "0.2.0".into(), "0.3.1".into())]);
        assert!(missed.is_mismatched());
        assert_eq!(missed.unpinned(), ["soothfast 0.2.0 -> 0.3.1"]);
        assert_eq!(
            super::HarnessSync::from_unpinned(missed.unpinned().to_vec()),
            missed
        );
    }

    #[test]
    fn a_plain_directory_in_the_repo_is_not_a_worktree_root() {
        let Ok(root) = super::workspace_root() else {
            return;
        };
        assert!(!super::is_worktree_root(&root.join("cargo-soothfast")));
    }

    #[test]
    fn the_workspace_root_is_a_worktree_root_only_in_a_linked_worktree() {
        let Ok(root) = super::workspace_root() else {
            return;
        };
        assert_eq!(super::is_worktree_root(&root), root.join(".git").is_file());
    }
}
