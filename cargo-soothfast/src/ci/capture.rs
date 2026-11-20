//! `ci capture`: fails a docs package whose recorded example output has gone
//! stale. A package whose docs hold no capture block passes.

use super::each;
use super::env::CiEnv;
use std::path::Path;

use crate::config::{Config, Group};
use crate::{init, invoke};

/// `docs capture -p PKG [PATHS]`, which records fresh output.
pub(super) fn args(pkg: &str, paths: &[String]) -> Vec<String> {
    let mut args: Vec<String> = ["docs", "capture", "-p", pkg].map(String::from).to_vec();
    args.extend(paths.iter().cloned());
    args
}

fn command(pkg: &str, paths: &[String]) -> Vec<String> {
    let mut args = args(pkg, paths);
    args.insert(4, "--check".to_string());
    args
}

/// The docs packages whose docs can be told apart. A lone package owns every
/// doc. Among several, the blocks of all docs would compile into each
/// package's crate, so only a package with `paths` of its own is covered.
fn separable(
    docs: Vec<String>,
    own_paths: impl Fn(&str) -> Result<bool, String>,
) -> Result<Vec<String>, String> {
    if docs.len() <= 1 {
        return Ok(docs);
    }
    let mut kept = Vec::new();
    for pkg in docs {
        if own_paths(&pkg)? {
            kept.push(pkg);
        }
    }
    Ok(kept)
}

/// The docs packages `ci capture` covers and the docs paths it limits them
/// to: `[ci] capture_paths` for a lone package, since several each have paths
/// of their own. No paths means every doc in a package's scope.
pub(super) fn covered(root: &Path, config: &Config) -> Result<(Vec<String>, Vec<String>), String> {
    let docs = init::rust_plan(root)?.docs_packages;
    let paths = match docs.len() {
        1 => config.capture_paths()?,
        _ => Vec::new(),
    };
    let shared = config.resolve(&[], Group::Docs)?.paths;
    let packages = separable(docs, |pkg| {
        let own = config.resolve(&[pkg.to_string()], Group::Docs)?.paths;
        Ok(own.is_some() && own != shared)
    })?;
    Ok((packages, paths))
}

/// A covered package regeneration captures, and the `pages` its output lands
/// in.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Regenerated {
    pub package: String,
    pub paths: Vec<String>,
    pub pages: Vec<String>,
}

/// The packages regeneration captures: the covered ones whose docs hold a
/// capture block, none when `[ci] skip` names `capture`.
pub(crate) fn regenerated(root: &Path, config: &Config) -> Result<Vec<Regenerated>, String> {
    if config.ci()?.iter().any(|kind| kind == "capture") {
        return Ok(Vec::new());
    }
    let (packages, paths) = covered(root, config)?;
    let mut found = Vec::new();
    for package in packages {
        let pages = crate::docs::capture_pages(root, config, &package, &paths)?;
        if !pages.is_empty() {
            found.push(Regenerated {
                package,
                paths: paths.clone(),
                pages,
            });
        }
    }
    Ok(found)
}

pub(super) fn configured_at(root: &Path) -> Result<Vec<String>, String> {
    Ok(covered(root, &Config::load(root)?)?.0)
}

/// Entry point for `ci capture`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let none = "ci capture: no docs package with docs of its own";
    let root = invoke::project_root();
    let (packages, paths) = match Config::load(&root).and_then(|c| covered(&root, &c)) {
        Ok(covered) => covered,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    let found = || Ok(packages);
    each::run_packages(env, "capture", args, none, found, |pkg| {
        command(pkg, &paths)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_runs_capture_in_check_mode() {
        assert_eq!(
            command("core", &[]),
            ["docs", "capture", "-p", "core", "--check"]
        );
        assert_eq!(
            command("core", &["README.md".to_string()]).last().unwrap(),
            "README.md"
        );
    }

    #[test]
    fn several_docs_packages_need_paths_of_their_own() {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let own = |pkg: &str| Ok(pkg == "b");
        assert_eq!(separable(names(&["a"]), own).unwrap(), ["a"]);
        assert_eq!(separable(names(&["a", "b", "c"]), own).unwrap(), ["b"]);
        assert!(separable(Vec::new(), own).unwrap().is_empty());
    }
}
