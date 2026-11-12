//! `ci capture`: fails a docs package whose recorded example output has gone
//! stale. A package whose docs hold no capture block passes.

use super::each;
use super::env::CiEnv;
use std::path::Path;

use crate::config::{Config, Group};
use crate::{init, invoke};

fn command(pkg: &str, paths: &[String]) -> Vec<String> {
    let mut args: Vec<String> = ["docs", "capture", "-p", pkg, "--check"]
        .map(String::from)
        .to_vec();
    args.extend(paths.iter().cloned());
    args
}

/// `[ci] capture_paths`, which narrows the docs a lone docs package is
/// checked against. Several packages each have paths of their own.
fn lone_paths(root: &Path) -> Result<Vec<String>, String> {
    match init::rust_plan(root)?.docs_packages.len() {
        1 => Config::load(root)?.capture_paths(),
        _ => Ok(Vec::new()),
    }
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
    let paths = match lone_paths(&invoke::project_root()) {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    each::run_packages(env, "capture", args, none, configured, |pkg| {
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
