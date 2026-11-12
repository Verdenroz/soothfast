//! What init reads from the repo and the workflow it wrote before: the
//! default branch, the soothfast ref and the toolchain pins.

use std::path::Path;

use crate::{invoke, tagref};

/// The remote's HEAD, else the branch the workflow already pushes on, else
/// `main`. CI checkouts have no `origin/HEAD`, so the remote is asked unless
/// `offline`.
pub(super) fn default_branch(root: &Path, existing: &str, offline: bool) -> String {
    let git = |args: &[&str]| invoke::git_in(root, args).ok();
    git(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .and_then(|h| h.strip_prefix("origin/").map(String::from))
        .or_else(|| {
            (!offline)
                .then(|| git(&["ls-remote", "--symref", "origin", "HEAD"]))
                .flatten()
                .and_then(|t| symref_branch(&t))
        })
        .or_else(|| pushed_branch(existing))
        .unwrap_or_else(|| "main".into())
}

/// `ref: refs/heads/master\tHEAD` in `ls-remote --symref` output.
fn symref_branch(listing: &str) -> Option<String> {
    listing.lines().find_map(|l| {
        let (target, name) = l.strip_prefix("ref: ")?.split_once('\t')?;
        (name == "HEAD").then(|| target.strip_prefix("refs/heads/").map(String::from))?
    })
}

/// The branch a workflow init wrote pushes on.
fn pushed_branch(workflow: &str) -> Option<String> {
    let line = workflow
        .lines()
        .find(|l| l.trim_start().starts_with("branches: ["))?;
    let inner = line.split_once('[')?.1.split_once(']')?.0.trim();
    (!inner.is_empty()).then(|| inner.to_string())
}

/// The `--pin` ref, else the workflow's existing pin of this release or of a
/// bare commit, else the commit this release's tag names on the remote (not
/// asked when `offline`), else the tag.
pub(super) fn action_ref(existing: &str, pin: Option<&str>, offline: bool) -> String {
    if let Some(pin) = pin {
        return format!("Verdenroz/soothfast@{pin}");
    }
    let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    let pinned = existing.lines().find_map(|l| {
        let pin = l
            .trim()
            .trim_start_matches("- ")
            .strip_prefix("uses: ")?
            .trim();
        let pin = pin.replacen("/.github/workflows/run.yml", "", 1);
        let commit = pin
            .strip_prefix("Verdenroz/soothfast@")
            .is_some_and(|r| r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit()));
        (pin.starts_with("Verdenroz/soothfast@") && (pin.ends_with(&format!(" # {tag}")) || commit))
            .then_some(pin)
    });
    if let Some(pin) = pinned {
        return pin;
    }
    let sha = (!offline)
        .then(|| tagref::tag_sha("Verdenroz/soothfast", &tag))
        .flatten();
    match sha {
        Some(sha) => format!("Verdenroz/soothfast@{sha} # {tag}"),
        None => format!("Verdenroz/soothfast@{tag}"),
    }
}

/// The value of `key: "..."` in a workflow init wrote.
pub(super) fn pinned(workflow: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}: \"");
    workflow.lines().find_map(|l| {
        let rest = l.trim().strip_prefix(&prefix)?;
        Some(rest.strip_suffix('"')?.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::test_support::ws;
    use crate::init::workflow::workflow;
    #[test]
    fn the_branch_comes_from_the_remote_or_the_workflow_already_written() {
        let listing = "ref: refs/heads/master\tHEAD\n0123\tHEAD\n";
        assert_eq!(symref_branch(listing).as_deref(), Some("master"));
        assert_eq!(symref_branch("0123\tHEAD\n"), None);
        let text = workflow(&ws(&[], None));
        assert_eq!(pushed_branch(&text).as_deref(), Some("master"));
    }

    #[test]
    fn an_existing_pin_of_this_release_is_kept() {
        let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
        let pin = format!("Verdenroz/soothfast@{} # {tag}", "c".repeat(40));
        let existing = format!("    steps:\n      - uses: {pin}\n");
        assert_eq!(action_ref(&existing, None, true), pin);
    }

    #[test]
    fn a_pin_overrides_and_a_bare_commit_pin_is_kept() {
        assert_eq!(
            action_ref("", Some("abc123"), true),
            "Verdenroz/soothfast@abc123"
        );
        let sha = "e".repeat(40);
        let existing = format!("    uses: Verdenroz/soothfast/.github/workflows/run.yml@{sha}\n");
        assert_eq!(
            action_ref(&existing, None, true),
            format!("Verdenroz/soothfast@{sha}")
        );
    }
}
