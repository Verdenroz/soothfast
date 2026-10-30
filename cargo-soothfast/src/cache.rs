//! `cargo soothfast cache merge --from DIR`: fold a restored `.soothfast`
//! cache layout into this project's, so one run can start from the union of
//! two saved caches.

use std::path::Path;

use crate::{itemcache, linkmap, runcache, rustdocstore};

pub fn run(args: &[String]) -> i32 {
    let [sub, flag, from] = args else {
        return err("usage: cache merge --from DIR");
    };
    if sub != "merge" || flag != "--from" {
        return err("usage: cache merge --from DIR");
    }
    let from = Path::new(from);
    if let Some(items) = runcache::state_dir("items") {
        itemcache::merge(&from.join("items"), &items);
    }
    for (name, keep) in [("runs", runcache::KEEP), ("maps", linkmap::KEEP)] {
        if let Some(dir) = runcache::state_dir(name) {
            copy_missing(&from.join(name), &dir);
            runcache::prune_oldest(&dir, keep);
        }
    }
    if let Some(dir) = runcache::state_dir("rustdoc") {
        copy_missing(&from.join("rustdoc"), &dir);
        rustdocstore::prune(&dir, rustdocstore::MAX_BYTES);
    }
    0
}

/// Copy each file of `from` that `into` does not have. Runs, link maps and
/// rustdoc JSON are named by what they were made from, so a name that exists
/// is the same file.
fn copy_missing(from: &Path, into: &Path) {
    let Ok(files) = std::fs::read_dir(from) else {
        return;
    };
    for file in files.flatten() {
        let dest = into.join(file.file_name());
        if !dest.exists() {
            let _ = std::fs::copy(file.path(), dest);
        }
    }
}

fn err(msg: &str) -> i32 {
    eprintln!("cache: {msg}");
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_absent_files_are_copied() {
        let root = std::env::temp_dir().join(format!("soothfast-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (from, into) = (root.join("from"), root.join("into"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&into).unwrap();
        std::fs::write(from.join("a"), "from").unwrap();
        std::fs::write(from.join("b"), "from").unwrap();
        std::fs::write(into.join("a"), "into").unwrap();
        copy_missing(&from, &into);
        assert_eq!(std::fs::read_to_string(into.join("a")).unwrap(), "into");
        assert_eq!(std::fs::read_to_string(into.join("b")).unwrap(), "from");
    }

    #[test]
    fn a_malformed_command_is_a_usage_error() {
        assert_eq!(run(&[]), 2);
        assert_eq!(run(&["merge".into(), "--to".into(), "x".into()]), 2);
    }
}
