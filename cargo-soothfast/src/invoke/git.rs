//! Running git in the workspace root or another directory.

use std::io;
use std::path::Path;
use std::process::Command;

use super::workspace_root;

/// Run a git command in the workspace root, returning trimmed stdout.
pub fn git(args: &[&str]) -> io::Result<String> {
    let root = workspace_root()?;
    git_in(&root, args)
}

/// Run a git command in `dir`, returning trimmed stdout.
pub fn git_in(dir: &Path, args: &[&str]) -> io::Result<String> {
    let out = Command::new("git").args(args).current_dir(dir).output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
