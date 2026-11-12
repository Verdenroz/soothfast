//! The outcome of the last `cargo soothfast test` run, which `status` reads.

use std::path::Path;

use serde_json::json;

use crate::invoke;

/// Where the record lives, relative to the project root.
pub(crate) const FILE: &str = ".soothfast/test-status.json";

/// `HEAD`'s tree and a digest of the uncommitted changes on top of it, so a
/// record can say whether it still describes the files on disk.
pub(crate) fn snapshot(root: &Path) -> Option<String> {
    let tree = invoke::git_in(root, &["rev-parse", "HEAD^{tree}"]).ok()?;
    let diff = invoke::git_in(root, &["diff", "HEAD", "--binary"]).ok()?;
    let untracked = invoke::git_in(
        root,
        &[
            "ls-files",
            "-o",
            "--exclude-standard",
            "--",
            ".",
            ":!.soothfast",
        ],
    )
    .ok()?;
    let digest = soothfast_registry::fnv1a(format!("{diff}\0{untracked}").as_bytes());
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
