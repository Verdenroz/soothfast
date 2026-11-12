//! `ci coverage`: holds each docs package to the documented-API minimum its
//! configuration sets.

use super::each;
use super::env::CiEnv;
use crate::config::{self, Group};
use crate::{init, invoke};

fn command(pkg: &str) -> Vec<String> {
    ["coverage", "docs", "-p", pkg].map(String::from).to_vec()
}

/// The packages that have a minimum to meet.
fn with_minimum(
    packages: Vec<String>,
    minimum: impl Fn(&str) -> Result<Option<u32>, String>,
) -> Result<Vec<String>, String> {
    let mut kept = Vec::new();
    for pkg in packages {
        if minimum(&pkg)?.is_some() {
            kept.push(pkg);
        }
    }
    Ok(kept)
}

fn configured() -> Result<Vec<String>, String> {
    let docs = init::rust_plan(&invoke::project_root())?.docs_packages;
    with_minimum(docs, |pkg| {
        Ok(config::defaults(&[pkg.to_string()], Group::Docs)?.coverage)
    })
}

/// Entry point for `ci coverage`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let none = "ci coverage: no docs package sets a coverage minimum";
    each::run_packages(env, "coverage", args, none, configured, command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_runs_coverage_docs() {
        assert_eq!(command("core"), ["coverage", "docs", "-p", "core"]);
    }

    #[test]
    fn only_packages_with_a_minimum_are_kept() {
        let packages = ["a", "b", "c"].map(String::from).to_vec();
        let kept = with_minimum(packages, |pkg| Ok((pkg != "b").then_some(80)));
        assert_eq!(kept, Ok(vec!["a".to_string(), "c".to_string()]));
    }

    #[test]
    fn a_config_error_is_reported() {
        let packages = vec!["a".to_string()];
        assert!(with_minimum(packages, |_| Err("bad".into())).is_err());
    }
}
