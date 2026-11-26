//! Which public items tests and benches execute, from `-C instrument-coverage`
//! builds mapped onto rustdoc spans.

use std::path::PathBuf;

use crate::invoke::CommonArgs;

mod export;
mod instrument;
mod regions;
mod tools;

pub use regions::Regions;

/// The regions the package's tests execute. Doc tests are not instrumented.
pub fn tests(pkg: &str, features: Option<&str>) -> Result<Regions, String> {
    let tools = tools::locate()?;
    let build = instrument::Instrumented::new(pkg, features, tools::host_triple()?)?;
    let executables = instrument::build_tests(&build)?;
    let profraw = scratch("tests");
    let ran = std::fs::create_dir_all(&profraw)
        .map_err(|e| e.to_string())
        .and_then(|()| instrument::run_into(build.tests(false), &profraw))
        .and_then(|()| export::regions(&tools, &executables, &profraw));
    let _ = std::fs::remove_dir_all(&profraw);
    ran
}

/// The regions the package's bench target executes, one pass over each item.
pub fn benches(common: &CommonArgs) -> Result<Regions, String> {
    let pkg = common
        .pkg
        .as_deref()
        .ok_or("coverage executed requires -p PKG")?;
    let target = common.target.as_deref().unwrap_or("soothfast");
    let tools = tools::locate()?;
    let build =
        instrument::Instrumented::new(pkg, common.features.as_deref(), tools::host_triple()?)?;
    let executable = instrument::build_bench(&build, target)?;
    let profraw = scratch("benches");
    let ran = std::fs::create_dir_all(&profraw)
        .map_err(|e| e.to_string())
        .and_then(|()| instrument::run_into(build.benches(target, false), &profraw))
        .and_then(|()| export::regions(&tools, &[executable], &profraw));
    let _ = std::fs::remove_dir_all(&profraw);
    ran
}

fn scratch(what: &str) -> PathBuf {
    std::env::temp_dir().join(format!("soothfast-coverage-{what}-{}", std::process::id()))
}
