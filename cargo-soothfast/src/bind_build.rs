//! Shelling out to each language's own packaging tool.
//!
//! `maturin` drives its own build through `pyo3-build-config`, which knows
//! which interpreter to link against; `wasm-pack` drives cargo plus
//! wasm-bindgen's post-processor. C has no such tool, so that one is a plain
//! `cargo build`: the glue crate already declares both library kinds.

use std::path::Path;
use std::process::Command;

use soothfast_bind::BindKind;
use soothfast_sdk::target::Target;

use crate::sdk_build;

/// Build one entry's package, returning the artifacts it produced.
pub(crate) fn run(
    kind: BindKind,
    glue: &Path,
    targets: &[String],
    release: bool,
) -> Result<Vec<String>, String> {
    match kind {
        BindKind::Python => maturin(glue, targets, release),
        BindKind::Wasm => wasm_pack(glue, targets, release),
        BindKind::CAbi => cargo(glue, targets, release),
    }
}

/// One `cargo build` per target, or one untargeted build when none are
/// configured.
///
/// A target whose toolchain is missing is reported and skipped rather than
/// failing the run, the same way the SDK's matrix behaves: a machine rarely
/// carries every cross-linker, and the targets it does carry are still worth
/// building.
fn cargo(glue: &Path, targets: &[String], release: bool) -> Result<Vec<String>, String> {
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
        match compile_c(glue, target, release, profile) {
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
) -> Result<Vec<String>, String> {
    let mut args = vec!["build"];
    if release {
        args.push("--release");
    }
    if let Some(triple) = target {
        args.extend(["--target", triple]);
    }
    let status = Command::new("cargo")
        .args(&args)
        .current_dir(glue)
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err(match target {
            Some(triple) => format!(
                "cargo build failed — is the target installed? \
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

/// One `wasm-pack build`, whatever the configured targets say.
///
/// A `.wasm` has no os/cpu/libc axis, so the triples a Python package needs
/// mean nothing here and naming any is a mistake worth reporting.
fn wasm_pack(glue: &Path, targets: &[String], release: bool) -> Result<Vec<String>, String> {
    if !targets.is_empty() {
        eprintln!(
            "soothfast: ignoring --target for wasm; one .wasm runs on every \
             platform, so there is no matrix to build"
        );
    }
    let mut args = vec!["build", "--target", "web"];
    if release {
        args.push("--release");
    }
    let status = Command::new("wasm-pack")
        .args(&args)
        .current_dir(glue)
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "`wasm-pack` not found — cargo install wasm-pack".to_string()
            }
            _ => format!("cannot run wasm-pack: {e}"),
        })?;
    if !status.success() {
        return Err(format!("`wasm-pack {}` failed", args.join(" ")));
    }
    artifacts(&glue.join("pkg"), &["wasm", "js", "ts"])
}

/// One `maturin build` per target, or one untargeted build when none are
/// configured.
///
/// A target whose toolchain is missing is reported and skipped: a partial
/// matrix is a normal local outcome, and CI builds the full one.
fn maturin(glue: &Path, targets: &[String], release: bool) -> Result<Vec<String>, String> {
    let resolved = Target::matrix(targets)?;
    if !targets.is_empty()
        && let Some(warning) = sdk_build::manylinux_warning(&resolved)
    {
        eprintln!("soothfast: {warning}");
    }

    let mut failures = Vec::new();
    if targets.is_empty() {
        maturin_once(glue, None, release)?;
    } else {
        for target in &resolved {
            if let Err(e) = maturin_once(glue, Some(target.triple), release) {
                failures.push(format!("{}: {e}", target.triple));
            }
        }
    }
    for failure in &failures {
        eprintln!("soothfast: skipped {failure}");
    }
    artifacts(&glue.join("target/wheels"), &["whl"])
}

fn maturin_once(glue: &Path, triple: Option<&str>, release: bool) -> Result<(), String> {
    let mut args = vec!["build"];
    if release {
        args.push("--release");
    }
    if let Some(triple) = triple {
        args.extend(["--target", triple]);
    }
    let status = Command::new("maturin")
        .args(&args)
        .current_dir(glue)
        .status()
        .map_err(|e| match e.kind() {
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

/// What the build tool left behind. Reading the directory rather than
/// parsing the tool's output keeps this working across its release notes.
fn artifacts(dir: &Path, extensions: &[&str]) -> Result<Vec<String>, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Err(format!("nothing built in {}", dir.display()));
    };
    let mut out: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| extensions.iter().any(|e| x == *e))
        })
        .map(|p| p.display().to_string())
        .collect();
    out.sort();
    Ok(out)
}
