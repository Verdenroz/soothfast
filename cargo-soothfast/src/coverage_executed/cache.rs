//! Executed regions kept under `.soothfast/coverage/<digest>`, the digest
//! covering the bytes of the instrumented executables and the inputs the
//! test pass cache keys on. An unchanged tree runs nothing.

use std::path::{Path, PathBuf};

use super::regions::Regions;
use crate::config::Config;
use crate::invoke;
use crate::runcache;
use crate::test::{key, select};

/// The stored regions for `executables` run for `scope`, else the result of
/// `run`, stored. `pkg` is the package whose files the run reads.
pub fn through(
    scope: &str,
    pkg: &str,
    executables: &[PathBuf],
    run: impl FnOnce() -> Result<Regions, String>,
) -> Result<Regions, String> {
    let Some(dir) = runcache::state_dir("coverage") else {
        return run();
    };
    let file = dir.join(format!(
        "{}.json",
        key(scope, &environment(pkg)?, executables)?
    ));
    if let Some(regions) = load(&file) {
        return Ok(regions);
    }
    let regions = run()?;
    if std::fs::write(&file, regions.to_json().to_string()).is_ok() {
        runcache::prune_oldest(&dir, runcache::KEEP);
    }
    Ok(regions)
}

fn environment(pkg: &str) -> Result<String, String> {
    let root = invoke::workspace_root().map_err(|e| e.to_string())?;
    let cfg = Config::load(&root)?.test()?;
    let pkgs = select::packages(&root).map_err(|e| e.to_string())?;
    key::environment(&root, &pkgs, &cfg, pkg)
}

fn key(scope: &str, environment: &str, executables: &[PathBuf]) -> Result<String, String> {
    let mut parts = vec![
        "soothfast-coverage-v2".to_string(),
        scope.to_string(),
        environment.to_string(),
    ];
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
