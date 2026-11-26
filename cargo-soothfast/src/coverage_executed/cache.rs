//! Executed regions kept under `.soothfast/coverage/<digest>`, the digest
//! covering the bytes of the instrumented executables. An unchanged tree
//! rebuilds nothing and runs nothing.

use std::path::{Path, PathBuf};

use super::regions::Regions;
use crate::runcache;

/// The stored regions for `executables`, else the result of `run`, stored.
pub fn through(
    kind: &str,
    executables: &[PathBuf],
    run: impl FnOnce() -> Result<Regions, String>,
) -> Result<Regions, String> {
    let Some(dir) = runcache::state_dir("coverage") else {
        return run();
    };
    let file = dir.join(format!("{}.json", key(kind, executables)?));
    if let Some(regions) = load(&file) {
        return Ok(regions);
    }
    let regions = run()?;
    if std::fs::write(&file, regions.to_json().to_string()).is_ok() {
        runcache::prune_oldest(&dir, runcache::KEEP);
    }
    Ok(regions)
}

fn key(kind: &str, executables: &[PathBuf]) -> Result<String, String> {
    let mut parts = vec!["soothfast-coverage-v1".to_string(), kind.to_string()];
    for exe in executables {
        let bytes = std::fs::read(exe).map_err(|e| format!("{}: {e}", exe.display()))?;
        parts.push(runcache::hex_fnv(&bytes));
    }
    Ok(runcache::hashed(&parts))
}

fn load(file: &Path) -> Option<Regions> {
    let text = std::fs::read_to_string(file).ok()?;
    Regions::from_json(&serde_json::from_str(&text).ok()?)
}
