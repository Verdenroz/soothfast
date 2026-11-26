//! The outcome of the last `cargo soothfast test` run, which `status` reads.

use std::path::Path;

use serde_json::json;

use super::key;
use crate::invoke;

/// Where the record lives, relative to the project root.
pub(crate) const FILE: &str = ".soothfast/test-status.json";

/// `HEAD`'s tree and a digest of the uncommitted changes on top of it, so a
/// record can say whether it still describes the files on disk.
pub(crate) fn snapshot(root: &Path) -> Option<String> {
    let tree = invoke::git_in(root, &["rev-parse", "HEAD^{tree}"]).ok()?;
    let diff = invoke::git_in(root, &["diff", "HEAD", "--binary"]).ok()?;
    let listing = invoke::git_in(
        root,
        &[
            "-c",
            "core.quotepath=false",
            "ls-files",
            "-o",
            "--exclude-standard",
            "--",
            ".",
            ":!.soothfast",
        ],
    )
    .ok()?;
    let untracked: Vec<String> = listing
        .lines()
        .filter(|p| root.join(p).is_file())
        .map(String::from)
        .collect();
    let ids: Vec<String> = key::blob_ids(root, &untracked)
        .ok()?
        .into_iter()
        .map(|(path, id)| format!("{path}\u{0}{id}"))
        .collect();
    let digest = soothfast_registry::fnv1a(format!("{diff}\0{}", ids.join("\n")).as_bytes());
    Some(format!("{tree}:{digest:016x}"))
}

/// Write the record of a finished run.
pub(crate) fn record(root: &Path, failures: usize) {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let doc = json!({
        "passed": failures == 0,
        "failures": failures,
        "unix": unix,
        "snapshot": snapshot(root),
    });
    let path = root.join(FILE);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, doc.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::fixture::TempRepo;

    #[test]
    fn editing_an_untracked_file_moves_the_snapshot() {
        let repo = TempRepo::new("last-untracked");
        repo.write("a.rs", "fn a() {}");
        repo.git(&["add", "."]);
        repo.commit();
        repo.write("new.rs", "fn one() {}");
        let before = snapshot(&repo.root);
        repo.write("new.rs", "fn two() {}");
        assert_ne!(before, snapshot(&repo.root));
    }

    #[test]
    fn an_unchanged_tree_keeps_its_snapshot() {
        let repo = TempRepo::new("last-same");
        repo.write("a.rs", "fn a() {}");
        repo.git(&["add", "."]);
        repo.commit();
        repo.write("new.rs", "fn one() {}");
        repo.write(".soothfast/test-status.json", "{}");
        let before = snapshot(&repo.root);
        repo.write(".soothfast/test-status.json", "{\"passed\":true}");
        assert_eq!(before, snapshot(&repo.root));
    }
}
