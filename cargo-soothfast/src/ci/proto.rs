//! `ci proto`: reconciles each package's `[[proto]]` messages against the
//! structs that carry them.

use std::path::Path;

use super::each;
use super::env::CiEnv;
use crate::{invoke, spec_config};

fn command(pkg: &str) -> Vec<String> {
    ["spec", "check-proto", "-p", pkg]
        .map(String::from)
        .to_vec()
}

pub(super) fn configured_at(root: &Path) -> Result<Vec<String>, String> {
    spec_config::proto_packages(root)
}

fn configured() -> Result<Vec<String>, String> {
    configured_at(&invoke::workspace_root().map_err(|e| e.to_string())?)
}

/// Entry point for `ci proto`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let none = "ci proto: no package has a [[proto]] entry";
    each::run_packages(env, "proto", args, none, configured, command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_runs_check_proto() {
        assert_eq!(command("wire"), ["spec", "check-proto", "-p", "wire"]);
    }
}
