//! Raw profiles to executed regions: merge with `llvm-profdata`, export with
//! `llvm-cov`.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::regions::Regions;
use super::tools::Tools;

/// Sources of dependencies and the standard library carry no public item of
/// the package, and dominate the export if left in.
const IGNORED_SOURCES: &str = "/\\.cargo/registry/|/\\.rustup/|/rustc/";

/// The regions the profiles in `profraw` record for `executables`.
pub fn regions(tools: &Tools, executables: &[PathBuf], profraw: &Path) -> Result<Regions, String> {
    let merged = profraw.join("merged.profdata");
    merge(tools, profraw, &merged)?;
    let mut cmd = Command::new(&tools.cov);
    cmd.arg("export")
        .arg(format!("-instr-profile={}", merged.display()))
        .arg(format!("-ignore-filename-regex={IGNORED_SOURCES}"));
    for (i, exe) in executables.iter().enumerate() {
        if i > 0 {
            cmd.arg("-object");
        }
        cmd.arg(exe);
    }
    let out = cmd.output().map_err(|e| format!("llvm-cov: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "llvm-cov export failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let export = serde_json::from_slice(&out.stdout).map_err(|e| format!("llvm-cov: {e}"))?;
    let mut regions = Regions::default();
    regions.add_export(&export);
    Ok(regions)
}

fn merge(tools: &Tools, profraw: &Path, merged: &Path) -> Result<(), String> {
    let raw: Vec<PathBuf> = std::fs::read_dir(profraw)
        .map_err(|e| e.to_string())?
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "profraw"))
        .collect();
    if raw.is_empty() {
        return Err("the instrumented run wrote no profile data".into());
    }
    let out = Command::new(&tools.profdata)
        .args(["merge", "-sparse"])
        .args(&raw)
        .arg("-o")
        .arg(merged)
        .output()
        .map_err(|e| format!("llvm-profdata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "llvm-profdata merge failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}
