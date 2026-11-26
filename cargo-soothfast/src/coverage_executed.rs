//! Which public items tests and benches execute, from `-C instrument-coverage`
//! builds mapped onto rustdoc spans.

use std::path::PathBuf;
use std::process::Command;

use crate::invoke::CommonArgs;

mod cache;
mod cli;
mod export;
mod instrument;
mod regions;
mod tools;

pub use cli::run;
pub use regions::Regions;

/// The regions the package's tests execute. Doc tests are not instrumented.
pub fn tests(pkg: &str, features: Option<&str>) -> Result<Regions, String> {
    let tools = tools::locate()?;
    let build = instrument::Instrumented::new(pkg, features, tools::host_triple()?)?;
    let executables = instrument::build_tests(&build)?;
    if executables.is_empty() {
        return Ok(Regions::default());
    }
    let scope = format!("tests {pkg} {}", features.unwrap_or_default());
    cache::through(&scope, pkg, &executables, || {
        run_and_export(&tools, &executables, "tests", build.tests(false), false)
    })
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
    let executables = [executable];
    let scope = format!(
        "benches {pkg} {target} {}",
        common.features.as_deref().unwrap_or_default()
    );
    cache::through(&scope, pkg, &executables, || {
        run_and_export(
            &tools,
            &executables,
            "benches",
            build.benches(target, false),
            true,
        )
    })
}

fn run_and_export(
    tools: &tools::Tools,
    executables: &[PathBuf],
    what: &str,
    run: Command,
    lenient: bool,
) -> Result<Regions, String> {
    let profraw =
        std::env::temp_dir().join(format!("soothfast-coverage-{what}-{}", std::process::id()));
    let regions = std::fs::create_dir_all(&profraw)
        .map_err(|e| e.to_string())
        .and_then(|()| instrument::run_into(run, &profraw, lenient))
        .and_then(|complete| {
            let mut regions = export::regions(tools, executables, &profraw)?;
            regions.partial = !complete;
            Ok(regions)
        });
    let _ = std::fs::remove_dir_all(&profraw);
    regions
}
