//! Running built test targets through cargo, which owns the test
//! environment (`[env]` config, target runners, library paths).

use std::io;
use std::process::Command;

use crate::testbuild::TestBin;

/// Run one already-built target through `cargo test`, output streaming.
/// Whether it exited successfully.
pub fn run_target(bin: &TestBin, cargo_args: &[String], run_args: &[String]) -> io::Result<bool> {
    let mut cmd = Command::new("cargo");
    cmd.arg("test").args(["-p", &bin.package]);
    cmd.args(cargo_args).args(bin.target_flags());
    if !run_args.is_empty() {
        cmd.arg("--").args(run_args);
    }
    Ok(cmd.status()?.success())
}

/// `cargo test --doc` for `names`; executables cannot be cached, so doc tests
/// always run.
pub fn doc_tests(names: &[String], cargo_args: &[String], run_args: &[String]) -> io::Result<bool> {
    let mut cmd = Command::new("cargo");
    cmd.args(["test", "--doc"]);
    for n in names {
        cmd.args(["-p", n]);
    }
    cmd.args(cargo_args);
    if !run_args.is_empty() {
        cmd.arg("--").args(run_args);
    }
    Ok(cmd.status()?.success())
}

/// Whether cargo flags already pick test targets, which `--doc` refuses to
/// combine with.
pub fn narrows_targets(cargo_args: &[String]) -> bool {
    const FLAGS: &[&str] = &[
        "--lib",
        "--bin",
        "--bins",
        "--test",
        "--tests",
        "--example",
        "--examples",
        "--bench",
        "--benches",
        "--all-targets",
    ];
    cargo_args
        .iter()
        .any(|a| FLAGS.contains(&a.split('=').next().unwrap_or(a)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn target_selecting_flags_are_detected() {
        assert!(narrows_targets(&args(&["--release", "--lib"])));
        assert!(narrows_targets(&args(&["--test=golden"])));
        assert!(narrows_targets(&args(&["--bin=cli"])));
        assert!(!narrows_targets(&args(&["--all-features", "--release"])));
    }
}
