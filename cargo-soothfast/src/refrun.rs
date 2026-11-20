//! Where `report changelog` gets the measurement of the release it compares
//! against: HEAD's own baseline, a stored report, a run the gate recorded, or
//! a worktree build.

use std::path::Path;

use crate::invoke;

/// The commit a reference measurement stands for, relative to HEAD.
#[derive(Debug, PartialEq)]
pub enum Anchor {
    /// The reference is HEAD itself.
    Head,
    /// The merge-base of HEAD and the reference; empty when git has none.
    At(String),
}

/// The merge-base of HEAD and `refname` in the repository at `dir`.
pub fn anchor(dir: &Path, refname: &str) -> Anchor {
    let sha = invoke::git_in(dir, &["merge-base", "HEAD", refname]).unwrap_or_default();
    match invoke::git_in(dir, &["rev-parse", "HEAD"]) {
        Ok(head) if head == sha => Anchor::Head,
        _ => Anchor::At(sha),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{Anchor, anchor, invoke};

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn repo(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("soothfast-refrun-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "one"]);
        dir
    }

    #[test]
    fn a_tag_on_head_anchors_at_head() {
        let dir = repo("at-head");
        git(&dir, &["tag", "v1"]);
        assert_eq!(anchor(&dir, "v1"), Anchor::Head);
    }

    #[test]
    fn a_tag_behind_head_anchors_at_its_commit() {
        let dir = repo("behind");
        git(&dir, &["tag", "v1"]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "two"]);
        let tagged = invoke::git_in(&dir, &["rev-parse", "v1"]).unwrap();
        assert_eq!(anchor(&dir, "v1"), Anchor::At(tagged));
    }
}
