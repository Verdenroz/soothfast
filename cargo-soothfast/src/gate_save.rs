//! What a gate's `--save-baseline` writes. A passing gate saves every number
//! measured from code proven identical to HEAD's: counters from a stored run
//! or the item cache count, walltime from either does not and is listed
//! `unmeasured` per metric instead. Regressions alone still save HEAD's
//! numbers. A gate whose claims failed, or that errored, saves no numbers,
//! only a `gate-failed` entry naming the package's items. A `--shard` gate
//! also stamps the file for `baseline merge`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::baseline_merge;
use crate::headrun;
use crate::invoke::{self, CommonArgs, Reused, Run};
use crate::unmeasured::{self, Reason, Unmeasured};
use crate::workspace;

/// Where a gate's HEAD run came from.
#[derive(Debug, PartialEq)]
pub enum HeadRun {
    /// Measured by this gate, gating counters included. Items spliced from
    /// the item cache say so in their own `reused`.
    Measured,
    /// The identical-binaries pass, which took no gating counters.
    Uncounted,
    /// Read whole from the run cache, labelled with what it was measured from.
    Stored(String),
}

/// The metric a reused item's walltime is reported under.
const WALLTIME: &str = "walltime.median_ns";

/// The package a `--save-baseline` is recorded under: `-p`'s, or the
/// workspace's one bench package. A save with several and no `-p` cannot be
/// attributed, so it is refused before anything is measured or written.
pub fn save_package(
    common: &CommonArgs,
    crates: impl FnOnce() -> Result<Vec<String>, String>,
) -> Result<String, String> {
    if let Some(pkg) = &common.pkg {
        return Ok(pkg.clone());
    }
    let crates = crates().map_err(|e| {
        format!(
            "--save-baseline needs -p PKG: the workspace's bench packages could not be read ({e})"
        )
    })?;
    match crates.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err("--save-baseline needs -p PKG: the workspace has no bench package".into()),
        many => Err(format!(
            "--save-baseline needs -p PKG in a workspace with more than one bench package ({})",
            many.join(", ")
        )),
    }
}

/// Resolve a save's package into `common`, then `configure` it, so a config
/// error still has a package to mark. A buildcost save needs `-p`, as its
/// gate does; it is refused here, before anything is built. The outer error
/// is the package's, the inner one the config's.
pub fn configure_save<T>(
    common: &mut CommonArgs,
    saving: bool,
    crates: impl FnOnce(&CommonArgs) -> Result<Vec<String>, String>,
    configure: impl FnOnce(&mut CommonArgs) -> Result<T, String>,
) -> Result<Result<T, String>, String> {
    if saving {
        let pkg = match (common.backend.as_deref(), &common.pkg) {
            (Some("buildcost"), Some(pkg)) => pkg.clone(),
            (Some("buildcost"), None) => return Err("--backend buildcost requires -p PKG".into()),
            _ => save_package(common, || crates(common))?,
        };
        common.pkg = Some(pkg);
    }
    Ok(configure(common))
}

/// The workspace's bench packages for `common`'s bench target.
pub fn bench_packages(common: &CommonArgs) -> Result<Vec<String>, String> {
    workspace::bench_crates(common.target.as_deref().unwrap_or("soothfast"), None)
}

/// Act on `--save-baseline NAME` under `root` for a gate in which
/// `failed_claims` of HEAD's claims failed. Regressions alone still save
/// HEAD's numbers: they are measured, and the gate's own verdict, or a label
/// accepting it, decides pass or fail. `listed` is every item the bench
/// binary lists, if it was read. Under a shard only its own ids are saved.
/// `common.pkg` is set: [`configure_save`] resolved it before the gate ran.
pub fn save_head(
    root: &Path,
    name: &str,
    current: &Run,
    source: &HeadRun,
    common: &CommonArgs,
    listed: Option<&[String]>,
    failed_claims: u32,
) -> Result<(), String> {
    let buildcost = common.backend.as_deref() == Some("buildcost");
    let Some(package) = common.pkg.as_deref() else {
        return Err("--save-baseline needs -p PKG".into());
    };
    if failed_claims > 0 && buildcost {
        eprintln!("soothfast: refusing to save baseline {name:?}: a claim failed");
        return Ok(());
    }
    if failed_claims > 0 {
        eprintln!("soothfast: saving no numbers in baseline {name:?}: a claim failed");
        let reason = Reason::GateFailed;
        return mark_package(root, name, common, reason, "a claim failed", listed);
    }
    if let (Some(shard), None) = (common.shard, listed) {
        let reason = Reason::GateFailed;
        mark_package(root, name, common, reason, "listing failed", None)?;
        return Err(format!(
            "cannot list the bench binary's items, which shard {shard}'s baseline must carry"
        ));
    }
    if let Some(shard) = common.shard {
        baseline_merge::check_shard_in(root, name, shard)?;
    }
    let (mut run, mut entries) = baseline_material(current, source, package);
    if let Some(shard) = common.shard {
        run.items.retain(|id, _| shard.owns(id));
        run.assertions.retain(|a| shard.owns_claim(&a.id));
        for e in &mut entries {
            e.items.retain(|id| shard.owns(id));
        }
        entries.retain(|e| !e.items.is_empty());
    }
    let scope = invoke::SaveScope::of(common);
    let path = invoke::save_baseline_in(root, name, &run, scope).map_err(|e| e.to_string())?;
    unmeasured::append_in(root, name, &entries).map_err(|e| e.to_string())?;
    println!("baseline saved: {}", path.display());
    report(name, &entries);
    stamp(root, name, common, listed.unwrap_or_default())
}

/// The ids of `listed` this gate owns: all of them, or its shard's.
fn owned_here(common: &CommonArgs, listed: &[String]) -> Vec<String> {
    listed
        .iter()
        .filter(|id| common.shard.is_none_or(|s| s.owns(id)))
        .cloned()
        .collect()
}

/// `--save-baseline NAME` for a gate that errored before it had a verdict.
/// Without a package nothing can be attributed, so nothing is written.
pub fn save_errored(
    root: &Path,
    name: &str,
    common: &CommonArgs,
    listed: Option<&[String]>,
    why: &str,
) -> Result<(), String> {
    if common.pkg.is_none() {
        return Ok(());
    }
    let detail = format!("the gate errored: {why}");
    mark_package(root, name, common, Reason::GateFailed, &detail, listed)
}

fn stamp(root: &Path, name: &str, common: &CommonArgs, listed: &[String]) -> Result<(), String> {
    match (common.shard, &common.pkg) {
        (Some(shard), Some(pkg)) => baseline_merge::stamp_shard_in(root, name, shard, pkg, listed)
            .map_err(|e| e.to_string()),
        _ => Ok(()),
    }
}

/// Replace `-p`'s numbers in baseline `name` with one `reason` entry, naming
/// the items of `listed` this gate owns, else those its prior `owners` name.
/// With neither it names nothing, and claims on its items fail.
pub fn mark_package(
    root: &Path,
    name: &str,
    common: &CommonArgs,
    reason: Reason,
    detail: &str,
    listed: Option<&[String]>,
) -> Result<(), String> {
    let Some(package) = common.pkg.as_deref() else {
        return Err(format!("cannot mark baseline {name:?} without -p PKG"));
    };
    if let Some(shard) = common.shard {
        baseline_merge::check_shard_in(root, name, shard)?;
    }
    let prior = invoke::load_baseline_in(root, name)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let filter = common.filter.as_deref();
    let items: Vec<String> = match listed {
        Some(ids) => owned_here(common, ids),
        None => unmeasured::owned_by(&prior, package)
            .into_iter()
            .filter(|id| filter.is_none_or(|f| id.contains(f)))
            .collect(),
    };
    let detail = match (&items[..], listed) {
        ([], None) => format!("{detail}, and no listing was available to name {package}'s items"),
        _ => format!("{detail}, so {package} saved no numbers"),
    };
    let entry = unmeasured::entry(package, reason, &detail, &[], items);
    report(name, std::slice::from_ref(&entry));
    unmeasured::mark_in(root, name, std::slice::from_ref(&entry), filter.is_none())?;
    stamp(root, name, common, listed.unwrap_or_default())
}

fn report(name: &str, entries: &[Unmeasured]) {
    for e in entries {
        println!(
            "gate: baseline {name:?} lists {} item(s) of {} as {} ({}): {}",
            e.items.len(),
            e.package,
            e.reason.as_str(),
            e.metrics.join(", "),
            e.detail
        );
    }
}

/// What of `current` a baseline may hold, and the entries for what it may
/// not. Walltime another run measured is dropped, and so are the verdicts on
/// those items; a complexity verdict is kept only where this run took the
/// counters.
fn baseline_material(current: &Run, source: &HeadRun, package: &str) -> (Run, Vec<Unmeasured>) {
    let stored = match source {
        HeadRun::Stored(from) => Some(from),
        _ => None,
    };
    let mut items = BTreeMap::new();
    let mut reused_wall = Vec::new();
    for (id, m) in &current.items {
        let mut m = m.clone();
        if let Some(from) = stored {
            m.reused = Some(Reused {
                from: from.clone(),
                metrics: headrun::reused_metrics(&m, true),
            });
        }
        let wall_reused = m
            .reused
            .as_ref()
            .is_some_and(|r| r.metrics.contains(&WALLTIME));
        if wall_reused && m.median_ns.is_some() {
            m.median_ns = None;
            m.mad_ns = None;
            m.p99_ns = None;
            m.wall_rounds.clear();
            reused_wall.push(id.clone());
        }
        if let Some(r) = &mut m.reused {
            r.metrics.retain(|metric| *metric != WALLTIME);
        }
        items.insert(id.clone(), m);
    }
    let counted_here = *source == HeadRun::Measured && current.reused.is_none();
    let assertions = current
        .assertions
        .iter()
        .filter(|a| !reused_wall.contains(&a.id))
        .filter(|a| counted_here || a.kind != "complexity")
        .cloned()
        .collect();
    let mut entries = Vec::new();
    if !reused_wall.is_empty() {
        let detail = match stored {
            Some(from) => {
                format!("HEAD's run came from the run cache ({from}); its walltime is that run's")
            }
            None => "spliced from the item cache; its walltime is another run's".to_string(),
        };
        entries.push(unmeasured::entry(
            package,
            Reason::ReusedWalltime,
            &detail,
            &["walltime"],
            reused_wall.clone(),
        ));
    }
    if *source == HeadRun::Uncounted {
        entries.push(unmeasured::entry(
            package,
            Reason::IdenticalBinaries,
            "bench binaries identical to the merge-base, and no stored run of them held the gating counters",
            &["perfcnt", "callgrind"],
            current.items.keys().cloned().collect(),
        ));
    }
    let run = Run {
        noise_pct: current.noise_pct,
        build: current.build.clone(),
        gating_backend: current.gating_backend.clone(),
        items,
        assertions,
        executed: BTreeMap::new(),
        reused: current.reused.clone(),
    };
    (run, entries)
}

#[cfg(test)]
mod tests;
