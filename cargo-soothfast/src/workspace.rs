//! Which crates of this cargo workspace a package's types can come from.
//!
//! Spec generation walks one package's rustdoc JSON, but its wire types
//! routinely live in a sibling crate of the same workspace. Those crates are
//! the only ones worth documenting for that purpose: a registry dependency
//! has no source tree here, and documenting the whole dependency graph would
//! cost a nightly rustdoc build per crate for types no route ever names.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

/// The workspace package names under `dir` from `cargo metadata --no-deps`,
/// read once per process.
pub fn package_names(dir: &Path) -> Result<BTreeSet<String>, String> {
    static NAMES: OnceLock<Mutex<HashMap<std::path::PathBuf, BTreeSet<String>>>> = OnceLock::new();
    let mut memo = NAMES
        .get_or_init(Default::default)
        .lock()
        .map_err(|e| e.to_string())?;
    if let Some(names) = memo.get(dir) {
        return Ok(names.clone());
    }
    let meta = metadata(Some(dir), true).map_err(|e| e.to_string())?;
    let names: BTreeSet<String> = (meta["packages"].as_array().into_iter().flatten())
        .filter_map(|p| p["name"].as_str().map(str::to_string))
        .collect();
    memo.insert(dir.to_path_buf(), names.clone());
    Ok(names)
}

/// `cargo metadata` for a tree: with the resolved dependency graph, or with
/// `no_deps` exactly the workspace's own packages and their declared
/// dependencies.
pub fn metadata(dir: Option<&Path>, no_deps: bool) -> io::Result<Value> {
    let mut cmd = Command::new("cargo");
    cmd.arg("metadata");
    if no_deps {
        cmd.arg("--no-deps");
    }
    cmd.args(["--format-version", "1"]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// Every member of this workspace, in name order. `--no-deps` lists exactly
/// the workspace's own packages, so no dependency can reach the result.
pub fn members(dir: Option<&Path>) -> io::Result<Vec<String>> {
    let md = metadata(dir, true)?;
    let mut names: Vec<String> = md["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["name"].as_str().map(String::from))
        .collect();
    names.sort();
    Ok(names)
}

/// Workspace members `pkg` depends on, transitively, in name order.
///
/// Only members with a plain library target qualify: a proc-macro crate's
/// rustdoc JSON describes macros rather than wire types, and a binary-only
/// member has no `--lib` for rustdoc to document. Dev- and build-only
/// dependencies are excluded — nothing a handler signature names can be
/// reachable through either.
///
/// The order is alphabetical rather than graph order, so two crates that
/// both define a type of the same name always disambiguate the same way.
pub fn local_deps(pkg: &str, dir: Option<&Path>) -> io::Result<Vec<String>> {
    collect(pkg, &metadata(dir, false)?)
}

/// The walk itself, over metadata already in hand — the seam tests use.
fn collect(pkg: &str, meta: &Value) -> io::Result<Vec<String>> {
    let packages: Vec<&Value> = meta["packages"].as_array().into_iter().flatten().collect();
    let members: BTreeSet<&str> = meta["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();

    let root = packages
        .iter()
        .find(|p| p["name"].as_str() == Some(pkg))
        .and_then(|p| p["id"].as_str())
        .ok_or_else(|| io::Error::other(format!("no workspace package named {pkg:?}")))?;

    let mut seen: BTreeSet<&str> = BTreeSet::from([root]);
    let mut queue = VecDeque::from([root]);
    let mut found = BTreeSet::new();
    while let Some(id) = queue.pop_front() {
        for dep in normal_deps(meta, id) {
            if !members.contains(dep) || !seen.insert(dep) {
                continue;
            }
            queue.push_back(dep);
            if let Some(p) = packages.iter().find(|p| p["id"].as_str() == Some(dep))
                && let (Some(name), true) = (p["name"].as_str(), has_lib_target(p))
            {
                found.insert(name.to_string());
            }
        }
    }
    Ok(found.into_iter().collect())
}

/// Whether `pkg` in the tree at `dir` has a `[[bench]]` target named
/// `target`.
///
/// The gate asks this of a merge-base worktree: a bench target that does
/// not exist there yet means the crate is newly measured, which is a
/// different fact from a bench that failed to run.
pub fn has_bench_target(pkg: &str, target: &str, dir: Option<&Path>) -> io::Result<bool> {
    Ok(bench_target_in(&metadata(dir, false)?, pkg, target))
}

/// The lookup itself, over metadata already in hand — the seam tests use.
pub(crate) fn bench_target_in(meta: &Value, pkg: &str, target: &str) -> bool {
    meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["name"].as_str() == Some(pkg))
        .flat_map(|p| p["targets"].as_array().into_iter().flatten())
        .any(|t| {
            t["name"].as_str() == Some(target)
                && t["kind"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|k| k.as_str() == Some("bench"))
        })
}

/// Every member with a bench target named `target`, by package name, in
/// name order: the packages a gate without `-p` runs. Read once per process
/// and tree; a failed read is asked again.
pub fn bench_crates(target: &str, dir: Option<&Path>) -> Result<Vec<String>, String> {
    type Key = (String, Option<std::path::PathBuf>);
    static CRATES: OnceLock<Mutex<HashMap<Key, Vec<String>>>> = OnceLock::new();
    let key = (target.to_string(), dir.map(Path::to_path_buf));
    let mut memo = CRATES
        .get_or_init(Default::default)
        .lock()
        .map_err(|e| e.to_string())?;
    if let Some(crates) = memo.get(&key) {
        return Ok(crates.clone());
    }
    let meta = metadata(dir, true).map_err(|e| e.to_string())?;
    let crates = bench_crates_in(&meta, target);
    memo.insert(key, crates.clone());
    Ok(crates)
}

/// The lookup itself, over metadata already in hand.
pub(crate) fn bench_crates_in(meta: &Value, target: &str) -> Vec<String> {
    let mut crates: Vec<String> = meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["name"].as_str())
        .filter(|package| bench_target_in(meta, package, target))
        .map(str::to_string)
        .collect();
    crates.sort();
    crates
}

/// The ids one resolve node depends on outside dev- and build-only edges.
fn normal_deps<'a>(meta: &'a Value, id: &str) -> Vec<&'a str> {
    let Some(node) = meta["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|n| n["id"].as_str() == Some(id))
    else {
        return Vec::new();
    };
    node["deps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| {
            // A null `kind` is cargo's spelling of a normal dependency.
            d["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k["kind"].is_null())
        })
        .filter_map(|d| d["pkg"].as_str())
        .collect()
}

/// Whether a package has a library target `cargo rustdoc --lib` can document.
fn has_lib_target(pkg: &Value) -> bool {
    pkg["targets"].as_array().into_iter().flatten().any(|t| {
        t["kind"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|k| matches!(k.as_str(), Some("lib" | "rlib" | "dylib")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lib(name: &str) -> Value {
        json!({ "name": name, "id": name, "targets": [{ "kind": ["lib"], "name": name }] })
    }

    fn meta(packages: Vec<Value>, members: Vec<&str>, nodes: Vec<Value>) -> Value {
        json!({ "packages": packages, "workspace_members": members,
                "resolve": { "nodes": nodes } })
    }

    fn node(id: &str, deps: Vec<(&str, Value)>) -> Value {
        let deps: Vec<Value> = deps
            .into_iter()
            .map(|(pkg, kinds)| json!({ "pkg": pkg, "dep_kinds": kinds }))
            .collect();
        json!({ "id": id, "deps": deps })
    }

    fn normal() -> Value {
        json!([{ "kind": Value::Null }])
    }

    #[test]
    fn a_registry_dependency_is_never_returned() {
        let m = meta(
            vec![lib("server"), lib("core"), lib("serde")],
            vec!["server", "core"],
            vec![node(
                "server",
                vec![("core", normal()), ("serde", normal())],
            )],
        );
        assert_eq!(collect("server", &m).unwrap(), vec!["core".to_string()]);
    }

    #[test]
    fn transitive_workspace_members_come_along() {
        let m = meta(
            vec![lib("server"), lib("core"), lib("model")],
            vec!["server", "core", "model"],
            vec![
                node("server", vec![("core", normal())]),
                node("core", vec![("model", normal())]),
            ],
        );
        assert_eq!(
            collect("server", &m).unwrap(),
            vec!["core".to_string(), "model".to_string()]
        );
    }

    #[test]
    fn dev_and_build_only_edges_are_skipped() {
        let dev = json!([{ "kind": "dev" }]);
        let m = meta(
            vec![lib("server"), lib("fixtures")],
            vec!["server", "fixtures"],
            vec![node("server", vec![("fixtures", dev)])],
        );
        assert!(collect("server", &m).unwrap().is_empty());
    }

    #[test]
    fn a_proc_macro_member_has_no_lib_to_document() {
        let macros = json!({ "name": "macros", "id": "macros",
                             "targets": [{ "kind": ["proc-macro"], "name": "macros" }] });
        let m = meta(
            vec![lib("server"), macros],
            vec!["server", "macros"],
            vec![node("server", vec![("macros", normal())])],
        );
        assert!(collect("server", &m).unwrap().is_empty());
    }

    #[test]
    fn a_dependency_cycle_terminates() {
        let m = meta(
            vec![lib("a"), lib("b")],
            vec!["a", "b"],
            vec![
                node("a", vec![("b", normal())]),
                node("b", vec![("a", normal())]),
            ],
        );
        assert_eq!(collect("a", &m).unwrap(), vec!["b".to_string()]);
    }

    /// The gate asks this of a merge-base worktree, where a crate that is
    /// newly benched has no such target yet.
    #[test]
    fn a_bench_target_is_found_only_on_the_package_that_has_one() {
        let benched = json!({ "name": "server", "id": "server", "targets": [
            { "kind": ["lib"], "name": "server" },
            { "kind": ["bench"], "name": "soothfast" }] });
        let m = meta(vec![benched, lib("plain")], vec!["server", "plain"], vec![]);
        assert!(bench_target_in(&m, "server", "soothfast"));
        assert!(!bench_target_in(&m, "plain", "soothfast"));
        // A bench under a different name is not the one the gate runs.
        assert!(!bench_target_in(&m, "server", "criterion"));
        assert!(!bench_target_in(&m, "absent", "soothfast"));
    }

    #[test]
    fn the_bench_crates_are_the_members_with_the_bench_target_by_package_name() {
        let renamed = json!({ "name": "my-server", "id": "my-server", "targets": [
            { "kind": ["lib"], "name": "wire" },
            { "kind": ["bench"], "name": "soothfast" }] });
        let m = meta(
            vec![renamed, lib("plain")],
            vec!["my-server", "plain"],
            vec![],
        );
        assert_eq!(bench_crates_in(&m, "soothfast"), ["my-server"]);
    }

    #[test]
    fn an_unknown_package_is_an_error() {
        let m = meta(vec![lib("server")], vec!["server"], vec![]);
        assert!(collect("nope", &m).is_err());
    }
}
