use std::path::Path;
use std::process::Command;

use soothfast_sdk::target::Target;

use super::staging::{artifacts, hush};
use crate::sdk_build;

/// One `maturin build` per target, or one untargeted build when none are
/// configured.
///
/// A target whose toolchain is missing is reported and skipped: a partial
/// matrix is a normal local outcome, and CI builds the full one. If every
/// target fails, this errors instead of reporting whatever wheels an
/// earlier run happened to leave behind.
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

    // Cleared first: a wheel from an earlier run must not be reported as
    // this run's output.
    let wheels = glue.join("target/wheels");
    let _ = std::fs::remove_dir_all(&wheels);

    let mut failures = Vec::new();
    if targets.is_empty() {
        maturin_once(glue, None, release, quiet)?;
    } else {
        for target in &resolved {
            if let Err(e) = maturin_once(glue, Some(target.triple), release, quiet) {
                failures.push(format!("{}: {e}", target.triple));
            }
        }
        if failures.len() == resolved.len() {
            return Err(format!("every target failed:\n  {}", failures.join("\n  ")));
        }
    }
    for failure in &failures {
        eprintln!("soothfast: skipped {failure}");
    }
    artifacts(&wheels, &["whl"])
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
            "`maturin` not found: pip install maturin, or uv tool install maturin".to_string()
        }
        _ => format!("cannot run maturin: {e}"),
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`maturin {}` failed", args.join(" ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bind_build::test_support::{copy_dir, lock};

    #[test]
    #[ignore = "shells out to cargo and maturin"]
    fn every_target_failing_errors_instead_of_reporting_a_stale_wheel() {
        let bind_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../soothfast-bind/tests");
        let scratch = std::env::temp_dir().join(format!(
            "soothfast-bind-maturin-build-smoke-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        copy_dir(&bind_dir.join("fixture_crate"), &scratch);
        let glue = scratch.join("glue");
        copy_dir(&bind_dir.join("goldens/python"), &glue);
        lock(&glue);

        let wheels = glue.join("target/wheels");
        std::fs::create_dir_all(&wheels).expect("makes wheels dir");
        let stale = wheels.join("stale-0.0.0-py3-none-any.whl");
        std::fs::write(&stale, b"stale").expect("writes stale wheel");

        // No dev machine has an aarch64 Windows cross toolchain installed,
        // so this target reliably fails without needing maturin absent.
        let err = maturin(&glue, &["aarch64-pc-windows-msvc".to_string()], false, true)
            .expect_err("an unbuildable target must fail, not report the stale wheel");
        assert!(err.contains("aarch64-pc-windows-msvc"), "{err}");
        assert!(
            !stale.exists(),
            "the stale wheel must be cleared, not reported"
        );
    }
}
