//! Which public items tests and benches execute, from `-C instrument-coverage`
//! builds mapped onto rustdoc spans.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use soothfast_docs::surface::Surface;

use crate::invoke::CommonArgs;

mod cache;
mod cli;
mod export;
mod instrument;
mod regions;
mod tools;

pub use cache::Cached;
pub use cli::run;
pub use regions::Regions;

/// The paths of the surface's public items a run can execute.
pub fn declared_paths(surface: &Surface) -> BTreeSet<String> {
    regions::declared(surface)
        .into_iter()
        .map(|d| d.path)
        .collect()
}

/// The declared items of `surface` that `regions` execute, by path.
pub fn reach(surface: &Surface, root: &Path, regions: &Regions) -> BTreeSet<String> {
    let declared = regions::declared(surface);
    regions::executed(&declared, root, regions)
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// The regions the package's tests execute. Doc tests are not instrumented.
pub fn tests(pkg: &str, features: Option<&str>) -> Result<Regions, String> {
    let tools = tools::locate()?;
    let build = instrument::Instrumented::new(pkg, features, tools::host_triple()?)?;
    let executables = instrument::build_tests(&build)?;
    if executables.is_empty() {
        return Ok(Regions::default());
    }
    cache::through(&tests_scope(pkg, features), pkg, &executables, || {
        run_and_export(&tools, &executables, "tests", build.tests(false), false)
    })
}

/// The stored [`tests`] run for the tree as it is now, if any.
pub fn cached_tests(pkg: &str, features: Option<&str>) -> Option<Cached> {
    cache::lookup(&tests_scope(pkg, features), pkg)
}

/// The stored [`benches`] run for the tree as it is now, if any.
pub fn cached_benches(common: &CommonArgs) -> Option<Cached> {
    let pkg = common.pkg.as_deref()?;
    let target = common.target.as_deref().unwrap_or("soothfast");
    cache::lookup(&benches_scope(pkg, target, common.features.as_deref()), pkg)
}

fn tests_scope(pkg: &str, features: Option<&str>) -> String {
    format!("tests {pkg} {}", features.unwrap_or_default())
}

fn benches_scope(pkg: &str, target: &str, features: Option<&str>) -> String {
    format!("benches {pkg} {target} {}", features.unwrap_or_default())
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
    cache::through(
        &benches_scope(pkg, target, common.features.as_deref()),
        pkg,
        &executables,
        || {
            run_and_export(
                &tools,
                &executables,
                "benches",
                build.benches(target, false),
                true,
            )
        },
    )
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
