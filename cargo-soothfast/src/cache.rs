//! `cargo soothfast cache merge --from DIR`: fold a restored `.soothfast`
//! cache layout into this project's, so one run can start from the union of
//! two saved caches.

use std::path::Path;

use crate::{binstore, itemcache, linkmap, runcache, rustdocstore, store};

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
        merge_rustdoc(&from.join("rustdoc"), &dir);
    }
    if let Some(dir) = runcache::state_dir("bins") {
        merge_bins(&from.join("bins"), &dir);
    }
    0
}

fn merge_rustdoc(from: &Path, into: &Path) {
    copy_missing(from, into);
    store::prune(into, rustdocstore::MAX_BYTES);
    store::pointer::merge(from, into);
}

fn merge_bins(from: &Path, into: &Path) {
    copy_missing(from, into);
    binstore::keep_newest(into);
    store::prune(into, binstore::MAX_BYTES);
    store::pointer::merge(from, into);
}

/// Copy each file of `from` that `into` does not have. Runs, link maps, bench
/// executables and rustdoc JSON are named by what they were made from, so a
/// name that exists is the same file. A copy is renamed into place, so an
/// interrupted one never leaves a truncated executable under a valid name.
fn copy_missing(from: &Path, into: &Path) {
    let Ok(files) = std::fs::read_dir(from) else {
        return;
    };
    for file in files.flatten() {
        let dest = into.join(file.file_name());
        if !dest.exists() {
            store::copy_atomic(&file.path(), &dest);
        }
    }
}

fn err(msg: &str) -> i32 {
    eprintln!("cache: {msg}");
    2
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::invoke::Visibility;
    use crate::store::testing::{Repo, inputs};

    const READ: [&str; 2] = ["app/src/lib.rs", "dep/src/lib.rs"];

    fn later() -> SystemTime {
        SystemTime::now() + Duration::from_secs(3600)
    }

    fn doc_key(repo: &Repo) -> String {
        let request = rustdocstore::Request {
            pkg: "app",
            features: None,
            visibility: Visibility::Private,
            rustc: "rustc 1.99.0-nightly",
            env: &[],
        };
        rustdocstore::key(&repo.0, &repo.dirs(), &request).unwrap()
    }

    fn bin_key(repo: &Repo) -> String {
        let request = binstore::Request {
            pkg: "app",
            target: "soothfast",
            profile: "bench",
            features: None,
            codegen_units: None,
            toolchain: None,
            rustc: "rustc 1.99.0",
            target_dir: Path::new("/t"),
            cargo_home: "/home/x/.cargo",
            env: &[],
        };
        binstore::key(&repo.0, &repo.dirs(), &request).unwrap()
    }

    fn document(repo: &Repo, dir: &Path, text: &str) {
        let json = repo.0.join("app.json");
        std::fs::write(&json, text).unwrap();
        let key = doc_key(repo);
        store::insert(dir, &key, &repo.0, &inputs(&READ), later(), &json, "json").unwrap();
    }

    fn build(repo: &Repo, dir: &Path, text: &str) {
        let exe = repo.0.join("bench");
        std::fs::write(&exe, text).unwrap();
        let key = bin_key(repo);
        let now = Some(key.clone());
        binstore::store(dir, &key, now, &repo.0, &inputs(&READ), later(), &exe, "g").unwrap();
    }

    fn read_doc(repo: &Repo, dir: &Path) -> Option<String> {
        let hit = store::lookup(dir, &doc_key(repo), &repo.0, "json")?;
        std::fs::read_to_string(hit).ok()
    }

    fn read_bin(repo: &Repo, dir: &Path) -> Option<String> {
        let hit = binstore::load(dir, &bin_key(repo), &repo.0)?;
        std::fs::read_to_string(hit).ok()
    }

    /// The store of the previous commit and the store another job built for
    /// the current one, with the tree left at the current commit.
    fn two_commits(tag: &str, store: fn(&Repo, &Path, &str)) -> (Repo, PathBuf, PathBuf) {
        let repo = Repo::new(tag);
        let (old, new) = (repo.0.join("old"), repo.0.join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        store(&repo, &old, "old");
        repo.write("app/src/lib.rs", "pub fn app() -> u8 { 1 }\n");
        store(&repo, &new, "new");
        (repo, old, new)
    }

    #[test]
    fn a_document_of_this_commit_merged_into_an_older_store_is_what_a_fallback_finds() {
        let (repo, old, new) = two_commits("merge-doc", document);
        merge_rustdoc(&new, &old);
        repo.write("CHANGELOG.md", "# changes\n");
        repo.write("app/openapi.yaml", "openapi: 3.1.0\n");
        assert_eq!(read_doc(&repo, &old).as_deref(), Some("new"));
    }

    #[test]
    fn an_older_document_merged_into_a_newer_store_leaves_its_pointer() {
        let (repo, old, new) = two_commits("merge-doc-back", document);
        merge_rustdoc(&old, &new);
        repo.write("CHANGELOG.md", "# changes\n");
        assert_eq!(read_doc(&repo, &new).as_deref(), Some("new"));
    }

    #[test]
    fn an_executable_of_this_commit_merged_into_an_older_store_is_what_a_fallback_finds() {
        let (repo, old, new) = two_commits("merge-bin", build);
        merge_bins(&new, &old);
        repo.write("app/soothfast-gate.lock", "pruned\n");
        repo.write("app/openapi.yaml", "openapi: 3.1.0\n");
        assert_eq!(read_bin(&repo, &old).as_deref(), Some("new"));
    }

    #[test]
    fn an_older_executable_merged_into_a_newer_store_leaves_its_pointer() {
        let (repo, old, new) = two_commits("merge-bin-back", build);
        merge_bins(&old, &new);
        repo.write("app/soothfast-gate.lock", "pruned\n");
        assert_eq!(read_bin(&repo, &new).as_deref(), Some("new"));
    }

    #[test]
    fn a_merged_pointer_whose_entry_is_absent_loses() {
        let (_repo, old, new) = two_commits("merge-absent", document);
        for file in std::fs::read_dir(&new).unwrap().flatten() {
            if file.path().extension().is_some_and(|x| x == "json") {
                std::fs::remove_file(file.path()).unwrap();
            }
        }
        let pointers = |dir: &Path| -> Vec<String> {
            let mut all: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .flatten()
                .filter(|f| f.path().extension().is_some_and(|x| x == "latest"))
                .map(|f| std::fs::read_to_string(f.path()).unwrap())
                .collect();
            all.sort();
            all
        };
        let before = pointers(&old);
        merge_rustdoc(&new, &old);
        assert_eq!(pointers(&old), before);
    }

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
