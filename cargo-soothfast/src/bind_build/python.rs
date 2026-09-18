use std::path::Path;
use std::process::Command;

use soothfast_sdk::target::Target;

use super::staging::{artifacts, hush};
use crate::sdk_build;

/// One `maturin build` per target, or one untargeted build when none are
/// configured.
///
/// A target whose toolchain is missing is reported and skipped: a partial
/// matrix is a normal local outcome, and CI builds the full one.
pub(super) fn maturin(
    glue: &Path,
    targets: &[String],
    release: bool,
    quiet: bool,
) -> Result<Vec<String>, String> {
    let resolved = Target::matrix(targets)?;
    if !targets.is_empty()
        && let Some(warning) = sdk_build::manylinux_warning(&resolved)
    {
        eprintln!("soothfast: {warning}");
    }

    let mut failures = Vec::new();
    if targets.is_empty() {
        maturin_once(glue, None, release, quiet)?;
    } else {
        for target in &resolved {
            if let Err(e) = maturin_once(glue, Some(target.triple), release, quiet) {
                failures.push(format!("{}: {e}", target.triple));
            }
        }
    }
    for failure in &failures {
        eprintln!("soothfast: skipped {failure}");
    }
    artifacts(&glue.join("target/wheels"), &["whl"])
}

fn maturin_once(
    glue: &Path,
    triple: Option<&str>,
    release: bool,
    quiet: bool,
) -> Result<(), String> {
    let mut args = vec!["build", "--locked"];
    if release {
        args.push("--release");
    }
    if let Some(triple) = triple {
        args.extend(["--target", triple]);
    }
    let mut cmd = Command::new("maturin");
    cmd.args(&args)
        .current_dir(glue)
        // Same reason as the C backend's cargo build: an inherited
        // CARGO_TARGET_DIR would move the wheel where `artifacts` below
        // never looks.
        .env_remove("CARGO_TARGET_DIR");
    hush(&mut cmd, quiet);
    let status = cmd.status().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            "`maturin` not found — pip install maturin, or uv tool install maturin".to_string()
        }
        _ => format!("cannot run maturin: {e}"),
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`maturin {}` failed", args.join(" ")))
    }
}
