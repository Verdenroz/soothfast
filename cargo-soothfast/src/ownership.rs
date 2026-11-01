//! Which package a saved item belongs to. A lib's measured fn is linked into
//! every dependent's bench binary, so each of their listings shows it; it
//! belongs to the package whose code registered it, when the runner names
//! that package and the package has a bench target of its own. Otherwise, as
//! with an older runner, it belongs to the package whose binary listed it.

use std::path::Path;

use crate::gate_save;
use crate::invoke::{self, CommonArgs, Listed, Run};

fn norm(pkg: &str) -> String {
    pkg.replace('-', "_")
}

/// Whether an item `registrar` registered belongs to a bench package other
/// than `pkg`.
pub fn foreign(registrar: Option<&str>, pkg: &str, bench: &[String]) -> bool {
    registrar.is_some_and(|r| norm(r) != norm(pkg) && bench.iter().any(|b| norm(b) == norm(r)))
}

/// The ids of `listing` that `pkg` owns.
pub fn owned_ids(listing: &[Listed], pkg: &str, bench: &[String]) -> Vec<String> {
    listing
        .iter()
        .filter(|(_, registrar)| !foreign(registrar.as_deref(), pkg, bench))
        .map(|(id, _)| id.clone())
        .collect()
}

/// `run` less the items, assertions and executed records of items `pkg`
/// does not own.
pub fn owned_run(run: &Run, pkg: &str, bench: &[String]) -> Run {
    let items: std::collections::BTreeMap<_, _> = run
        .items
        .iter()
        .filter(|(_, m)| !foreign(m.pkg.as_deref(), pkg, bench))
        .map(|(id, m)| (id.clone(), m.clone()))
        .collect();
    Run {
        noise_pct: run.noise_pct,
        build: run.build.clone(),
        gating_backend: run.gating_backend.clone(),
        assertions: (run.assertions.iter())
            .filter(|a| items.contains_key(&a.id))
            .cloned()
            .collect(),
        executed: (run.executed.iter())
            .filter(|(id, _)| items.contains_key(*id))
            .map(|(id, f)| (id.clone(), f.clone()))
            .collect(),
        reused: run.reused.clone(),
        items,
    }
}

/// The workspace's bench packages for `common`. None when they cannot be
/// read, which leaves every item with the package that listed it.
pub fn bench(common: &CommonArgs) -> Vec<String> {
    gate_save::bench_packages(common).unwrap_or_default()
}

/// `run` as `common`'s package saves it.
pub fn for_save(run: &Run, common: &CommonArgs) -> Run {
    match &common.pkg {
        Some(pkg) => owned_run(run, pkg, &bench(common)),
        None => owned_run(run, "", &[]),
    }
}

/// The ids `exe` lists under `common`'s filter that `common`'s package owns.
pub fn listed_owned(exe: &Path, common: &CommonArgs) -> Option<Vec<String>> {
    let listing = invoke::listing(exe, common.filter.as_deref())?;
    Some(match &common.pkg {
        Some(pkg) => owned_ids(&listing, pkg, &bench(common)),
        None => listing.into_iter().map(|(id, _)| id).collect(),
    })
}

#[cfg(test)]
mod tests;
