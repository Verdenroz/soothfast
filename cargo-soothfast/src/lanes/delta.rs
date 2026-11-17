//! The base side of the inputs a pull request touched.
//!
//! A fingerprint covers the head side of every input. A conflict resolved in
//! the pull request's favour can leave the head's inputs identical while the
//! base under them moved, so a pass is also keyed on what the touched inputs
//! were at the merge-base. Inputs the pull request did not touch equal the
//! base's by construction and add nothing.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use crate::invoke;
use crate::runcache::hashed;

const ABSENT: &str = "absent";

/// The digest of each of `paths` (sorted) with its blob id at `merge_base`,
/// or empty when no path is given. `None` when git cannot read the base.
pub fn base_delta(root: &Path, merge_base: &str, paths: &[&str]) -> Option<String> {
    if paths.is_empty() {
        return Some(String::new());
    }
    let ids = blob_ids(root, merge_base, paths).ok()?;
    Some(digest(paths, &ids))
}

fn digest(paths: &[&str], ids: &BTreeMap<String, String>) -> String {
    let mut sorted = paths.to_vec();
    sorted.sort_unstable();
    let parts: Vec<String> = sorted
        .iter()
        .map(|p| format!("{p}\u{0}{}", ids.get(*p).map_or(ABSENT, String::as_str)))
        .collect();
    hashed(&parts)
}

/// The blob id at `commit` of each of `paths` that exists there.
fn blob_ids(root: &Path, commit: &str, paths: &[&str]) -> io::Result<BTreeMap<String, String>> {
    let mut ids = BTreeMap::new();
    for chunk in paths.chunks(256) {
        let mut args = vec!["-c", "core.quotepath=false", "ls-tree", "-r", commit, "--"];
        args.extend(chunk);
        for line in invoke::git_in(root, &args)?.lines() {
            if let Some((meta, path)) = line.split_once('\t') {
                let id = meta.split_whitespace().nth(2).unwrap_or_default();
                ids.insert(path.to_string(), id.to_string());
            }
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::fixture::TempRepo;

    fn base(repo: &TempRepo) -> String {
        repo.git(&["rev-parse", "HEAD"])
    }

    fn repo(tag: &str) -> TempRepo {
        let repo = TempRepo::new(tag);
        repo.write("a.rs", "1");
        repo.write("b.rs", "1");
        repo.git(&["add", "."]);
        repo.commit();
        repo
    }

    #[test]
    fn no_touched_path_gives_an_empty_delta() {
        let repo = repo("delta-empty");
        assert_eq!(
            base_delta(&repo.root, &base(&repo), &[]),
            Some(String::new())
        );
    }

    #[test]
    fn the_head_side_of_a_touched_path_does_not_move_it() {
        let repo = repo("delta-head");
        let mb = base(&repo);
        let first = base_delta(&repo.root, &mb, &["a.rs"]).unwrap();
        repo.write("a.rs", "2");
        repo.git(&["add", "."]);
        repo.commit();
        assert_eq!(first, base_delta(&repo.root, &mb, &["a.rs"]).unwrap());
        assert!(!first.is_empty());
    }

    #[test]
    fn the_base_blob_moving_changes_it() {
        let repo = repo("delta-base");
        let before = base_delta(&repo.root, &base(&repo), &["a.rs"]).unwrap();
        repo.write("a.rs", "9");
        repo.git(&["add", "."]);
        repo.commit();
        assert_ne!(
            before,
            base_delta(&repo.root, &base(&repo), &["a.rs"]).unwrap()
        );
    }

    #[test]
    fn an_added_path_is_absent_and_the_order_of_paths_does_not_matter() {
        let repo = repo("delta-absent");
        let mb = base(&repo);
        let added = base_delta(&repo.root, &mb, &["new.rs"]).unwrap();
        let other = base_delta(&repo.root, &mb, &["other.rs"]).unwrap();
        assert_ne!(added, other);
        assert_ne!(added, base_delta(&repo.root, &mb, &["a.rs"]).unwrap());
        let ab = base_delta(&repo.root, &mb, &["a.rs", "b.rs"]);
        assert_eq!(ab, base_delta(&repo.root, &mb, &["b.rs", "a.rs"]));
    }

    #[test]
    fn a_deleted_path_is_read_from_the_base_and_an_unknown_base_is_none() {
        let repo = repo("delta-deleted");
        let mb = base(&repo);
        repo.git(&["rm", "-q", "a.rs"]);
        repo.commit();
        let gone = base_delta(&repo.root, &mb, &["a.rs"]).unwrap();
        assert_ne!(gone, base_delta(&repo.root, &mb, &["new.rs"]).unwrap());
        assert_eq!(base_delta(&repo.root, "no-such-ref", &["a.rs"]), None);
    }
}
