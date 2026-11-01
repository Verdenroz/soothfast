//! The registering package of each item a save writes. However a run was
//! assembled (measured, read from the run cache, or spliced from the item
//! cache), each item's `pkg` comes from the `--list --json` listing of the
//! binary that ran, which describes exactly that binary; a stored `pkg`
//! may be from an older CLI that had none. [`crate::ambiguity`] then
//! decides which package owns an id two of them list.

use std::collections::BTreeMap;
use std::path::Path;

use crate::invoke::{self, CommonArgs, Listed, Run};

/// `run` with each item's `pkg` taken from `listing`, `None` for an item
/// the listing does not name or names no package for.
pub fn stamped(run: &Run, listing: &[Listed]) -> Run {
    let pkgs: BTreeMap<&str, Option<&String>> = listing
        .iter()
        .map(|(id, pkg)| (id.as_str(), pkg.as_ref()))
        .collect();
    let mut items = run.items.clone();
    for (id, m) in &mut items {
        m.pkg = pkgs.get(id.as_str()).copied().flatten().cloned();
    }
    Run {
        noise_pct: run.noise_pct,
        build: run.build.clone(),
        gating_backend: run.gating_backend.clone(),
        items,
        assertions: run.assertions.clone(),
        executed: run.executed.clone(),
        reused: run.reused.clone(),
    }
}

/// `run` as a save writes it: stamped from the listing of `exe`, else of
/// `common`'s bench binary. With no listing to read, no item names a package.
pub fn for_save(run: &Run, common: &CommonArgs, exe: Option<&Path>) -> Run {
    let exe = exe
        .map(Path::to_path_buf)
        .or_else(|| invoke::bench_executable(common, None, None));
    let listing = exe
        .and_then(|exe| invoke::listing(&exe, common.filter.as_deref()))
        .unwrap_or_default();
    stamped(run, &listing)
}

#[cfg(test)]
mod tests;
