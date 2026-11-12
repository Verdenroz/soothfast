//! Workspace root, package lookup, and cargo target directories.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use serde_json::Value;

/// Workspace root (parent of the workspace Cargo.toml), via cargo itself.
/// Resolved once: `git()` calls this on every invocation, and the root
/// cannot move under a running process.
pub fn workspace_root() -> io::Result<PathBuf> {
    static ROOT: OnceLock<io::Result<PathBuf>> = OnceLock::new();
    match ROOT.get_or_init(locate_workspace_root) {
        Ok(root) => Ok(root.clone()),
        Err(e) => Err(io::Error::other(e.to_string())),
    }
}

fn locate_workspace_root() -> io::Result<PathBuf> {
    let out = Command::new("cargo")
        .args(["locate-project", "--workspace", "--message-format", "plain"])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("cargo locate-project failed"));
    }
    let manifest = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(PathBuf::from(manifest)
        .parent()
        .expect("manifest path has a parent")
        .to_path_buf())
}

/// The cargo workspace root, else the git toplevel, else the current
/// directory: the root for commands that run outside a cargo project.
pub fn project_root() -> PathBuf {
    workspace_root()
        .ok()
        .or_else(|| {
            let out = Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Directory of a workspace package (parent of its Cargo.toml).
pub fn pkg_dir(pkg: &str) -> io::Result<PathBuf> {
    pkg_meta(pkg).map(|m| m.dir)
}

/// A workspace package's directory and manifest fields, which stand in as
/// spec metadata defaults when `soothfast.toml` doesn't override them.
pub struct PkgMeta {
    pub dir: PathBuf,
    pub version: String,
    pub description: Option<String>,
}

pub fn pkg_meta(pkg: &str) -> io::Result<PkgMeta> {
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .stdout(Stdio::piped())
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("cargo metadata failed"));
    }
    let meta: Value = serde_json::from_slice(&out.stdout)?;
    let p = meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["name"] == pkg)
        .ok_or_else(|| io::Error::other(format!("no workspace package named {pkg:?}")))?;
    let dir = p["manifest_path"]
        .as_str()
        .map(|m| {
            PathBuf::from(m)
                .parent()
                .expect("manifest has parent")
                .to_path_buf()
        })
        .ok_or_else(|| io::Error::other(format!("package {pkg:?} has no manifest path")))?;
    Ok(PkgMeta {
        dir,
        version: p["version"].as_str().unwrap_or("0.0.0").to_string(),
        description: p["description"].as_str().map(String::from),
    })
}

/// Effective cargo target directory (respects CARGO_TARGET_DIR and
/// .cargo/config.toml), optionally for a build rooted in another worktree.
pub fn target_dir(dir: Option<&Path>) -> io::Result<PathBuf> {
    let mut cmd = Command::new("cargo");
    cmd.args(["metadata", "--no-deps", "--format-version", "1"]);
    cmd.stdout(Stdio::piped());
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(io::Error::other("cargo metadata failed"));
    }
    let meta: Value = serde_json::from_slice(&out.stdout)?;
    meta["target_directory"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("cargo metadata: no target_directory"))
}

/// Persistent target dir for merge-base worktree bench builds: a sibling
/// inside the parent's target dir, surviving the per-run worktree churn so
/// dependencies compile once per machine instead of once per gate. Sharing
/// the parent target dir itself is unsound: a workspace member hashes to
/// the same `-C metadata` from either path, so head and base leaf artifacts
/// would collide (and mtime races silently serve the wrong side's binary).
pub fn worktree_target_dir() -> io::Result<PathBuf> {
    Ok(target_dir(None)?.join("soothfast-worktree"))
}

/// Buildcost's own target dir. `cargo clean -p` drops every artifact of the
/// package under the profile dir whatever its hash, so a dir shared with the
/// bench gate would lose the gate's copies on each buildcost leg.
pub fn buildcost_target_dir() -> io::Result<PathBuf> {
    Ok(target_dir(None)?.join("soothfast-buildcost"))
}

/// The merge-base side of [`buildcost_target_dir`], kept beside the parent
/// target dir so it outlives the worktree the sources are checked out in.
/// Separate from the head dir for the reason [`worktree_target_dir`] gives:
/// a member hashes to the same `-C metadata` from either checkout.
pub fn buildcost_base_target_dir() -> io::Result<PathBuf> {
    Ok(target_dir(None)?.join("soothfast-buildcost-base"))
}
