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
    let inputs = environment(pkg)?;
    let file = dir.join(format!("{}.json", key(scope, &inputs, executables)?));
    let regions = match load(&file) {
        Some(regions) => regions,
        None => {
            let regions = run()?;
            store(&dir, &file, &regions);
            regions
        }
    };
    if file.exists() {
        point(scope, &inputs, &file);
    }
    Ok(regions)
}

/// A stored run and when it finished, as seconds since the epoch.
pub struct Cached {
    pub regions: Regions,
    pub unix: u64,
}

/// The stored run for `scope` whose inputs are those of the tree now, found
/// without building: a pointer keyed on the package's files, its workspace
/// dependencies', `Cargo.lock`, rustc and the scope names the entry
/// [`through`] stored.
pub fn lookup(scope: &str, pkg: &str) -> Result<Option<Cached>, String> {
    let (Some(dir), Some(at)) = (
        runcache::state_dir("coverage"),
        runcache::state_dir("coverage-inputs"),
    ) else {
        return Ok(None);
    };
    let pointer = at.join(pointer_key(scope, &environment(pkg)?));
    let Ok(name) = std::fs::read_to_string(pointer) else {
        return Ok(None);
    };
    let file = dir.join(name.trim());
    let unix = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Ok(load(&file).map(|regions| Cached { regions, unix }))
}

fn pointer_key(scope: &str, inputs: &str) -> String {
    runcache::hashed(&[
        "soothfast-coverage-at-v1".into(),
        scope.into(),
        inputs.into(),
    ])
}

fn point(scope: &str, inputs: &str, file: &Path) {
    let (Some(at), Some(name)) = (
        runcache::state_dir("coverage-inputs"),
        file.file_name().and_then(|n| n.to_str()),
    ) else {
        return;
    };
    if std::fs::write(at.join(pointer_key(scope, inputs)), name).is_ok() {
        runcache::prune_oldest(&at, runcache::KEEP);
    }
}

/// Only a run that finished is kept: a failed one stands for whatever ran
/// before it stopped, and would otherwise answer every later lookup.
fn store(dir: &Path, file: &Path, regions: &Regions) {
    if !regions.partial && std::fs::write(file, regions.to_json().to_string()).is_ok() {
        runcache::prune_oldest(dir, runcache::KEEP);
    }
}

fn environment(pkg: &str) -> Result<String, String> {
    let root = invoke::workspace_root().map_err(|e| e.to_string())?;
    let cfg = Config::load(&root)?.test()?;
    let pkgs = select::packages(&root).map_err(|e| e.to_string())?;
    key::environment_with_dependencies(&root, &pkgs, &cfg, pkg)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_run_is_not_stored() {
        let dir = std::env::temp_dir().join(format!("soothfast-cov-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (whole, partial) = (dir.join("whole.json"), dir.join("partial.json"));
        store(&dir, &whole, &Regions::default());
        let mut failed = Regions::default();
        failed.partial = true;
        store(&dir, &partial, &failed);
        assert!(load(&whole).is_some());
        assert!(!partial.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
