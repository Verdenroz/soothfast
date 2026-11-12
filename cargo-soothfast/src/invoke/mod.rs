//! Subprocess plumbing: run the user's soothfast bench binary via `cargo bench`,
//! parse its JSONL, and read/write baseline files.

use std::path::Path;
use std::time::Instant;

mod args;
mod baseline;
mod bench;
mod git;
mod harness;
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
pub(crate) use harness::lock_packages;
pub use harness::{HarnessSync, harness_versions, sync_harness_versions, tree_is_clean};
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
    use super::{forget_missing_worktree_in, git_in};

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
