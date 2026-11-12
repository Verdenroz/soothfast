//! The cargo workspace side of init: bench targets, docs packages and the
//! Rust plan.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use super::ci::Ci;
use super::target::ci;
use crate::{invoke, workspace};

const BENCH_STUB: &str = "\
use @@crate@@ as _;

soothfast::bench_main!();
";

const BENCH_TARGET: &str = "
[[bench]]
name = \"soothfast\"
harness = false
";

/// The cargo metadata of the workspace at `root`, after giving each of
/// `bench` a bench target unless only checking.
pub(super) fn rust_meta(root: &Path, bench: &[String], check: bool) -> Result<Value, String> {
    let load = || workspace::metadata(Some(root), true).map_err(|e| e.to_string());
    let meta = load()?;
    if check || bench.is_empty() {
        return Ok(meta);
    }
    for pkg in bench {
        add_bench(&meta, pkg)?;
    }
    load()
}

/// Members with a `soothfast` bench target.
pub(crate) fn benched(meta: &Value) -> Vec<String> {
    let mut names: Vec<String> = packages(meta)
        .filter_map(|p| p["name"].as_str())
        .filter(|name| workspace::bench_target_in(meta, name, "soothfast"))
        .map(String::from)
        .collect();
    names.sort();
    names
}

/// The packages `docs check` reads: `[docs] package` when set, else the
/// workspace packages the markdown binds name, else the root's own package.
pub(super) fn docs_packages(root: &Path, meta: &Value, ci: &Ci) -> Vec<String> {
    if let Some(packages) = &ci.docs {
        return packages.clone();
    }
    let listing = invoke::git_in(root, &["ls-files", "-z", "--", "*.md"]).unwrap_or_default();
    let prefixes: BTreeSet<String> = listing
        .split('\0')
        .filter(|path| !path.is_empty())
        .filter_map(|path| std::fs::read_to_string(root.join(path)).ok())
        .flat_map(|text| bind_crates(&text))
        .collect();
    let bound: Vec<String> = packages(meta)
        .filter_map(|p| p["name"].as_str())
        .filter(|name| prefixes.contains(&name.replace('-', "_")))
        .map(String::from)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if bound.is_empty() {
        docs_package(meta).into_iter().collect()
    } else {
        bound
    }
}

/// The crate part of each `crate::path` bind in `text`.
fn bind_crates(text: &str) -> Vec<String> {
    text.split("<!-- soothfast:bind ")
        .skip(1)
        .filter_map(|rest| rest.split_whitespace().next())
        .filter_map(|target| Some(target.split_once("::")?.0.to_string()))
        .collect()
}

/// The workspace root's own package, else the first member by name.
fn docs_package(meta: &Value) -> Option<String> {
    let root = meta["workspace_root"].as_str()?;
    let manifest = Path::new(root).join("Cargo.toml");
    let mut names: Vec<(bool, String)> = packages(meta)
        .filter_map(|p| {
            let at_root = p["manifest_path"].as_str().map(Path::new) == Some(manifest.as_path());
            Some((!at_root, p["name"].as_str()?.to_string()))
        })
        .collect();
    names.sort();
    names.into_iter().next().map(|(_, n)| n)
}

fn packages(meta: &Value) -> impl Iterator<Item = &Value> {
    meta["packages"].as_array().into_iter().flatten()
}

/// Whether any tracked markdown carries a soothfast marker for `docs check`.
pub fn has_markers(root: &Path) -> bool {
    invoke::git_in(root, &["grep", "-l", "--", "<!-- soothfast:", "*.md"])
        .is_ok_and(|s| !s.is_empty())
}

/// Give `pkg` a `soothfast` bench target: the dev-dependency, the
/// `[[bench]]` table and a stub that links the library's registrations.
fn add_bench(meta: &Value, pkg: &str) -> Result<(), String> {
    let p = packages(meta)
        .find(|p| p["name"] == pkg)
        .ok_or_else(|| format!("no workspace member named {pkg:?}"))?;
    let manifest = PathBuf::from(p["manifest_path"].as_str().unwrap_or_default());
    let dir = manifest.parent().ok_or("package has no directory")?;
    let text = std::fs::read_to_string(&manifest).map_err(|e| e.to_string())?;
    if !text.contains("name = \"soothfast\"") {
        std::fs::write(&manifest, format!("{text}{BENCH_TARGET}")).map_err(|e| e.to_string())?;
        println!("wrote    {} ([[bench]] soothfast)", manifest.display());
    }
    let version = env!("CARGO_PKG_VERSION");
    let status = Command::new("cargo")
        .args(["add", "--dev", "-p", pkg, "--features", "runner"])
        .arg(format!("soothfast@{version}"))
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("cargo add soothfast@{version} failed for {pkg}"));
    }
    let stub = dir.join("benches/soothfast.rs");
    if !stub.exists() {
        std::fs::create_dir_all(dir.join("benches")).map_err(|e| e.to_string())?;
        let krate = pkg.replace('-', "_");
        std::fs::write(&stub, BENCH_STUB.replace("@@crate@@", &krate))
            .map_err(|e| e.to_string())?;
        println!("wrote    {}", stub.display());
    }
    Ok(())
}

/// The packages and docs packages `plan` reads from a cargo workspace.
pub struct RustPlan {
    pub packages: Vec<String>,
    pub docs_packages: Vec<String>,
}

pub fn rust_plan(root: &Path) -> Result<RustPlan, String> {
    let meta = workspace::metadata(Some(root), true).map_err(|e| e.to_string())?;
    let docs = if has_markers(root) {
        docs_packages(root, &meta, &ci(root)?)
    } else {
        Vec::new()
    };
    Ok(RustPlan {
        packages: benched(&meta),
        docs_packages: docs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_bench_targets_named_soothfast_count() {
        let meta = json!({ "packages": [
            { "name": "b", "targets": [{ "name": "soothfast", "kind": ["bench"] }] },
            { "name": "a", "targets": [{ "name": "soothfast", "kind": ["bench"] }] },
            { "name": "c", "targets": [{ "name": "other", "kind": ["bench"] }] },
        ]});
        assert_eq!(benched(&meta), ["a", "b"]);
    }

    #[test]
    fn binds_name_the_crates_that_hold_their_items() {
        let text = "<!-- soothfast:bind my_core::a::B -->\nprose\n<!-- /soothfast:bind -->\n\
                    <!-- soothfast:bind other::c -->\n<!-- soothfast:bind src/x.py#f -->\n";
        assert_eq!(bind_crates(text), ["my_core", "other"]);
    }

    #[test]
    fn the_root_package_documents_the_workspace() {
        let meta = json!({ "workspace_root": "/w", "packages": [
            { "name": "a", "manifest_path": "/w/a/Cargo.toml" },
            { "name": "z", "manifest_path": "/w/Cargo.toml" },
        ]});
        assert_eq!(docs_package(&meta).as_deref(), Some("z"));
    }
}
