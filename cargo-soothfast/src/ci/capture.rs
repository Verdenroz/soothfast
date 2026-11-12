//! `ci capture`: fails a docs package whose recorded example output has gone
//! stale. A package whose docs hold no capture block passes.

use super::each;
use super::env::CiEnv;
use std::path::Path;

use crate::config::{Config, Group};
use crate::{init, invoke};

fn command(pkg: &str) -> Vec<String> {
    ["docs", "capture", "-p", pkg, "--check"]
        .map(String::from)
        .to_vec()
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

pub(super) fn configured_at(root: &Path) -> Result<Vec<String>, String> {
    let docs = init::rust_plan(root)?.docs_packages;
    let config = Config::load(root)?;
    let shared = config.resolve(&[], Group::Docs)?.paths;
    separable(docs, |pkg| {
        let own = config.resolve(&[pkg.to_string()], Group::Docs)?.paths;
        Ok(own.is_some() && own != shared)
    })
}

fn configured() -> Result<Vec<String>, String> {
    configured_at(&invoke::project_root())
}

/// Entry point for `ci capture`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let none = "ci capture: no docs package with docs of its own";
    each::run_packages(env, "capture", args, none, configured, command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_runs_capture_in_check_mode() {
        assert_eq!(
            command("core"),
            ["docs", "capture", "-p", "core", "--check"]
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
