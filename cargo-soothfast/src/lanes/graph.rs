//! The workspace's packages and what each can reach.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::test::select::{Package, parse_packages};
use crate::workspace;

/// The workspace packages of a tree whose every path dependency lives
/// inside it.
pub struct Graph {
    pkgs: Vec<Package>,
}

impl Graph {
    /// The packages of the workspace at `root`, or none when `cargo metadata`
    /// fails or a package or path dependency lies outside `root`, where no
    /// file this tree holds could say whether it changed.
    pub fn read(root: &Path) -> Option<Graph> {
        let meta = workspace::metadata(Some(root), true).ok()?;
        Graph::from_metadata(&meta, root)
    }

    pub(super) fn from_metadata(meta: &Value, root: &Path) -> Option<Graph> {
        let packages = meta["packages"].as_array()?;
        let inside = |path: Option<&str>| path.is_some_and(|p| Path::new(p).starts_with(root));
        let contained = packages.iter().all(|p| {
            let manifest = p["manifest_path"].as_str();
            let deps = p["dependencies"].as_array().into_iter().flatten();
            inside(manifest)
                && deps
                    .filter_map(|d| d["path"].as_str())
                    .all(|d| inside(Some(d)))
        });
        contained.then(|| Graph {
            pkgs: parse_packages(meta, root),
        })
    }

    pub fn packages(&self) -> &[Package] {
        &self.pkgs
    }

    /// `name` and every workspace package it reaches through a dependency of
    /// any kind, dev-dependencies included: a bench or a test builds them.
    /// Empty when the workspace has no such package.
    pub fn closure(&self, name: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut queue = vec![name];
        while let Some(next) = queue.pop() {
            let Some(pkg) = self.pkgs.iter().find(|p| p.name == next) else {
                continue;
            };
            if seen.insert(pkg.name.clone()) {
                queue.extend(pkg.deps.iter().map(String::as_str));
            }
        }
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn package(name: &str, deps: &[(&str, Option<&str>)]) -> Value {
        let deps: Vec<Value> = deps
            .iter()
            .map(|(n, path)| json!({ "name": n, "path": path }))
            .collect();
        json!({
            "name": name,
            "manifest_path": format!("/ws/{name}/Cargo.toml"),
            "dependencies": deps,
        })
    }

    fn graph(packages: Vec<Value>) -> Option<Graph> {
        Graph::from_metadata(&json!({ "packages": packages }), Path::new("/ws"))
    }

    fn names(set: BTreeSet<String>) -> Vec<String> {
        set.into_iter().collect()
    }

    #[test]
    fn the_closure_follows_every_dependency_kind_and_leaves_siblings_out() {
        let g = graph(vec![
            package("app", &[("lib", None), ("serde", None)]),
            package("lib", &[("fixtures", None)]),
            package("fixtures", &[]),
            package("sibling", &[("lib", None)]),
        ])
        .unwrap();
        assert_eq!(names(g.closure("app")), ["app", "fixtures", "lib"]);
        assert_eq!(names(g.closure("fixtures")), ["fixtures"]);
        assert!(g.closure("missing").is_empty());
    }

    #[test]
    fn a_dependency_cycle_ends() {
        let g = graph(vec![
            package("a", &[("b", None)]),
            package("b", &[("a", None)]),
        ])
        .unwrap();
        assert_eq!(names(g.closure("a")), ["a", "b"]);
    }

    #[test]
    fn a_path_dependency_outside_the_root_gives_no_graph() {
        let outside = package("app", &[("shared", Some("/elsewhere/shared"))]);
        assert!(graph(vec![outside]).is_none());
        let inside = package("app", &[("shared", Some("/ws/shared"))]);
        assert!(graph(vec![inside]).is_some());
    }

    #[test]
    fn a_package_outside_the_root_gives_no_graph() {
        let mut far = package("far", &[]);
        far["manifest_path"] = json!("/other/far/Cargo.toml");
        assert!(graph(vec![package("app", &[]), far]).is_none());
    }
}
