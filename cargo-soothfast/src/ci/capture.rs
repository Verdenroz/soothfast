//! `ci capture`: fails a docs package whose recorded example output has gone
//! stale. A package whose docs hold no capture block passes.

use super::each;
use super::env::CiEnv;
use crate::{init, invoke};

fn command(pkg: &str) -> Vec<String> {
    ["docs", "capture", "-p", pkg, "--check"]
        .map(String::from)
        .to_vec()
}

fn configured() -> Result<Vec<String>, String> {
    Ok(init::rust_plan(&invoke::project_root())?.docs_packages)
}

/// Entry point for `ci capture`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let none = "ci capture: no docs package";
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
}
