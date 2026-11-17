//! Which soothfast build runs the lanes.

use std::collections::BTreeMap;
use std::path::Path;

use super::graph::Graph;
use crate::runcache::hashed;
use crate::test::select::GLOBAL_FILES;
use crate::{glob, invoke};

const OWN_FILES: &[&str] = &["action/**", "action.yml", ".github/workflows/run.yml"];

/// The revision of the soothfast checkout, which names the tool. When that
/// checkout is the tree being planned (soothfast gating itself) every
/// revision is new, so the token is a digest of the files the tool is made
/// of instead. Empty when `rev` is.
pub fn token(root: &Path, rev: &str, files: &BTreeMap<String, String>, graph: &Graph) -> String {
    if rev.is_empty() || invoke::git_in(root, &["rev-parse", "HEAD"]).ok().as_deref() != Some(rev) {
        return rev.to_string();
    }
    let members: Vec<&str> = graph
        .packages()
        .iter()
        .map(|p| p.dir.as_str())
        .filter(|d| !d.is_empty())
        .collect();
    let made_of = |file: &str| {
        GLOBAL_FILES.contains(&file)
            || OWN_FILES.iter().any(|g| glob::matches(g, file))
            || members
                .iter()
                .any(|d| file.strip_prefix(d).is_some_and(|r| r.starts_with('/')))
    };
    let parts: Vec<String> = files
        .iter()
        .filter(|(f, _)| made_of(f))
        .map(|(f, id)| format!("{f}\u{0}{id}"))
        .collect();
    format!("own-{}", hashed(&parts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::fixture::TempRepo;
    use crate::test::project::Files;
    use serde_json::json;

    fn graph(repo: &TempRepo) -> Graph {
        let meta = json!({ "packages": [{
            "name": "tool",
            "manifest_path": format!("{}/tool/Cargo.toml", repo.root.display()),
            "dependencies": [],
        }]});
        Graph::from_metadata(&meta, &repo.root).unwrap()
    }

    fn committed(tag: &str) -> TempRepo {
        let repo = TempRepo::new(tag);
        for f in ["tool/src/main.rs", "action/run.sh", "README.md"] {
            repo.write(f, "1");
        }
        repo.git(&["add", "."]);
        repo.commit();
        repo
    }

    fn own(repo: &TempRepo) -> String {
        let head = repo.git(&["rev-parse", "HEAD"]);
        let files = Files::read(&repo.root).unwrap();
        token(&repo.root, &head, files.ids(), &graph(repo))
    }

    #[test]
    fn another_checkout_is_named_by_its_revision() {
        let repo = committed("lane-tool-rev");
        let files = Files::read(&repo.root).unwrap();
        let token = token(&repo.root, "abc123", files.ids(), &graph(&repo));
        assert_eq!(token, "abc123");
        assert_eq!(super::token(&repo.root, "", files.ids(), &graph(&repo)), "");
    }

    #[test]
    fn the_trees_own_checkout_is_named_by_the_files_the_tool_is_made_of() {
        let repo = committed("lane-tool-own");
        let first = own(&repo);
        assert!(first.starts_with("own-"));
        repo.write("README.md", "2");
        assert_eq!(first, own(&repo));
        repo.write("tool/src/main.rs", "2");
        let moved = own(&repo);
        assert_ne!(first, moved);
        repo.write("action/run.sh", "2");
        assert_ne!(moved, own(&repo));
    }
}
