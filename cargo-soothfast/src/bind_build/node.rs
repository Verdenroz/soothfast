use std::path::Path;
use std::process::Command;

use super::staging::{artifacts, hush};

/// `npm install` once, then one `napi build` per target, or one untargeted
/// build when none are configured.
///
/// A target whose toolchain is missing is reported and skipped, the same
/// posture as `maturin`'s matrix: a machine rarely carries every
/// cross-linker.
pub(super) fn napi(
    glue: &Path,
    targets: &[String],
    release: bool,
    quiet: bool,
) -> Result<Vec<String>, String> {
    if !glue.join("node_modules").exists() {
        npm_install(glue, quiet)?;
    }

    let mut failures = Vec::new();
    if targets.is_empty() {
        napi_build_once(glue, None, release, quiet)?;
    } else {
        for target in targets {
            if let Err(e) = napi_build_once(glue, Some(target), release, quiet) {
                failures.push(format!("{target}: {e}"));
            }
        }
    }
    for failure in &failures {
        eprintln!("soothfast: skipped {failure}");
    }
    artifacts(glue, &["node"])
}

fn npm_install(glue: &Path, quiet: bool) -> Result<(), String> {
    let mut cmd = Command::new("npm");
    cmd.arg("install").current_dir(glue);
    hush(&mut cmd, quiet);
    let status = cmd.status().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            "`npm` not found — install Node.js (https://nodejs.org)".to_string()
        }
        _ => format!("cannot run npm: {e}"),
    })?;
    match status.success() {
        true => Ok(()),
        false => Err("`npm install` failed".to_string()),
    }
}

fn napi_build_once(
    glue: &Path,
    target: Option<&str>,
    release: bool,
    quiet: bool,
) -> Result<(), String> {
    // `--platform` makes `napi build` emit the JS loader package.json's
    // `main` names; without it only the `.node` file lands. `napi build`
    // (@napi-rs/cli 3.9.1) has no `--locked` and no cargo passthrough.
    let mut args = vec!["napi", "build", "--platform"];
    if release {
        args.push("--release");
    }
    if let Some(triple) = target {
        args.extend(["--target", triple]);
    }
    let mut cmd = Command::new("npx");
    cmd.args(&args)
        .current_dir(glue)
        // napi build shells out to cargo itself; same CARGO_TARGET_DIR
        // reasoning as the C backend and maturin.
        .env_remove("CARGO_TARGET_DIR");
    hush(&mut cmd, quiet);
    let status = cmd.status().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            "`npm`/`npx` not found — install Node.js (https://nodejs.org)".to_string()
        }
        _ => format!("cannot run npx: {e}"),
    })?;
    match status.success() {
        true => Ok(()),
        false => Err(format!("`npx {}` failed", args.join(" "))),
    }
}
