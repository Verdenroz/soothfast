use std::path::Path;

use super::staging::{artifacts, hush};

/// One `cargo build` per target, or one untargeted build when none are
/// configured.
///
/// A target whose toolchain is missing is reported and skipped rather than
/// failing the run, the same way the SDK's matrix behaves: a machine rarely
/// carries every cross-linker, and the targets it does carry are still worth
/// building.
pub(super) fn cargo(
    glue: &Path,
    targets: &[String],
    release: bool,
    quiet: bool,
) -> Result<Vec<String>, String> {
    let profile = match release {
        true => "release",
        false => "debug",
    };
    let wanted: Vec<Option<&str>> = match targets.is_empty() {
        true => vec![None],
        false => targets.iter().map(|t| Some(t.as_str())).collect(),
    };
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for target in wanted {
        match compile_c(glue, target, release, profile, quiet) {
            Ok(found) => out.extend(found),
            Err(why) => skipped.push(format!("{}: {why}", target.unwrap_or("host"))),
        }
    }
    for line in &skipped {
        eprintln!("soothfast: skipping {line}");
    }
    if out.is_empty() {
        return Err(format!("no target built:\n  {}", skipped.join("\n  ")));
    }
    // The header describes every target, so it ships alongside all of them.
    out.extend(header(glue));
    out.sort();
    Ok(out)
}

fn compile_c(
    glue: &Path,
    target: Option<&str>,
    release: bool,
    profile: &str,
    quiet: bool,
) -> Result<Vec<String>, String> {
    let mut args = vec!["build", "--locked"];
    if release {
        args.push("--release");
    }
    if let Some(triple) = target {
        args.extend(["--target", triple]);
    }
    let mut cmd = crate::invoke::cargo_command();
    cmd.args(&args)
        .current_dir(glue)
        // The glue crate is its own workspace; an inherited CARGO_TARGET_DIR
        // would move its output where the artifact scan below never looks.
        .env_remove("CARGO_TARGET_DIR");
    hush(&mut cmd, quiet);
    let status = cmd.status().map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err(match target {
            Some(triple) => format!(
                "cargo build failed; is the target installed? \
                 (rustup target add {triple})"
            ),
            None => "cargo build failed".to_string(),
        });
    }
    let dir = match target {
        Some(triple) => glue.join("target").join(triple).join(profile),
        None => glue.join("target").join(profile),
    };
    let found = artifacts(&dir, &["so", "dylib", "dll", "a", "lib"])?;
    match found.is_empty() {
        true => Err(format!("built, but nothing landed in {}", dir.display())),
        false => Ok(found),
    }
}

/// The generated header, which a C consumer needs alongside the library.
fn header(glue: &Path) -> Vec<String> {
    artifacts(glue, &["h"]).unwrap_or_default()
}
