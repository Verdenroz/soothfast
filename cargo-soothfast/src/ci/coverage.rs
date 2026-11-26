//! `ci coverage`: reports every coverage dimension of each workspace library,
//! and fails a package only on a minimum its configuration sets.

use super::each;
use super::env::CiEnv;
use std::path::Path;

use crate::{invoke, workspace};

fn command(pkg: &str) -> Vec<String> {
    ["coverage", "report", "-p", pkg, "--check"]
        .map(String::from)
        .to_vec()
}

pub(super) fn configured_at(root: &Path) -> Result<Vec<String>, String> {
    let meta = workspace::metadata(Some(root), true).map_err(|e| e.to_string())?;
    Ok(workspace::lib_crates_in(&meta))
}

fn configured() -> Result<Vec<String>, String> {
    configured_at(&invoke::project_root())
}

/// Entry point for `ci coverage`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let none = "ci coverage: no workspace member has a library target";
    each::run_packages(env, "coverage", args, none, configured, command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_runs_a_checked_coverage_report() {
        assert_eq!(
            command("core"),
            ["coverage", "report", "-p", "core", "--check"]
        );
    }
}
