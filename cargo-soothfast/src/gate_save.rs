//! What a gate's `--save-baseline` writes. A passing gate saves every number
//! measured from code proven identical to HEAD's: counters from a stored run
//! or the item cache count, walltime from either does not and is listed
//! `unmeasured` per metric instead. A failing or errored gate saves no
//! numbers, only a `gate-failed` entry naming the package's items. A `--shard`
//! gate also stamps the file for `baseline merge`.

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

/// The package a save is recorded under: `-p`'s, as given to the gate.
/// Without one the entries still name their items; only the label is lost.
fn package_of(common: &CommonArgs) -> &str {
    common.pkg.as_deref().unwrap_or("workspace")
}

/// Act on `--save-baseline NAME` under `root` for a gate that ended with
/// `failures`. `listed` is every item the bench binary lists, if it was read.
pub fn save_head(
    root: &Path,
    name: &str,
    current: &Run,
    source: &HeadRun,
    common: &CommonArgs,
    listed: Option<&[String]>,
    failures: u32,
) -> Result<(), String> {
    if failures > 0 && common.backend.as_deref() == Some("buildcost") {
        eprintln!("soothfast: refusing to save baseline {name:?}: gate failed");
        return Ok(());
    }
    if failures > 0 {
        eprintln!("soothfast: saving no numbers in baseline {name:?}: gate failed");
        let reason = Reason::GateFailed;
        return mark_package(root, name, common, reason, "the gate failed", listed);
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
    let package = package_of(common);
    let owned: Vec<String> = match listed {
        Some(ids) => owned_here(common, ids),
        None => current.items.keys().cloned().collect(),
    };
    if common.pkg.is_some() {
        unmeasured::claim_in(root, name, package, &owned).map_err(|e| e.to_string())?;
    }
    let (run, entries) = baseline_material(current, source, package);
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
pub fn save_errored(
    root: &Path,
    name: &str,
    common: &CommonArgs,
    listed: Option<&[String]>,
    why: &str,
) -> Result<(), String> {
    let detail = format!("the gate errored: {why}");
    mark_package(root, name, common, Reason::GateFailed, &detail, listed)
}

fn stamp(root: &Path, name: &str, common: &CommonArgs, listed: &[String]) -> Result<(), String> {
    match (common.shard, &common.pkg) {
        (Some(shard), Some(pkg)) => baseline_merge::stamp_shard_in(root, name, shard, pkg, listed)
            .map_err(|e| e.to_string()),
        (Some(shard), None) => {
            baseline_merge::stamp_shard_in(root, name, shard, package_of(common), &[])
                .map_err(|e| e.to_string())
        }
        (None, _) => Ok(()),
    }
}

/// Replace the numbers of the gate's packages in baseline `name` with one
/// `reason` entry each. With `-p` that package is marked, naming the items
/// of `listed` this gate owns, else those its prior `owners` name, else none
/// (`all_items`). Without `-p` every bench package of the workspace is
/// marked by its prior `owners`.
pub fn mark_package(
    root: &Path,
    name: &str,
    common: &CommonArgs,
    reason: Reason,
    detail: &str,
    listed: Option<&[String]>,
) -> Result<(), String> {
    let target = common.target.as_deref().unwrap_or("soothfast");
    let crates = || workspace::bench_crates(target, None);
    mark_package_with(root, name, common, reason, detail, listed, crates)
}

/// [`mark_package`] with the workspace's bench packages read by `crates`.
/// When they cannot be read, every package the baseline's `owners` names is
/// marked instead and the error is returned; nothing is deleted.
pub(crate) fn mark_package_with(
    root: &Path,
    name: &str,
    common: &CommonArgs,
    reason: Reason,
    detail: &str,
    listed: Option<&[String]>,
    crates: impl FnOnce() -> Result<Vec<String>, String>,
) -> Result<(), String> {
    if let Some(shard) = common.shard {
        baseline_merge::check_shard_in(root, name, shard)?;
    }
    let prior = invoke::load_baseline_in(root, name)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let (packages, unread) = match (&common.pkg, crates) {
        (Some(pkg), _) => (vec![pkg.clone()], None),
        (None, crates) => match crates() {
            Ok(packages) => (packages, None),
            Err(e) => {
                let owners = prior[unmeasured::OWNERS_KEY].as_object();
                let known: Vec<String> = owners
                    .into_iter()
                    .flatten()
                    .map(|(p, _)| p.clone())
                    .collect();
                (known, Some(e))
            }
        },
    };
    let entries: Vec<Unmeasured> = packages
        .iter()
        .map(|package| {
            let items = match (listed, &common.pkg) {
                (Some(ids), Some(_)) => owned_here(common, ids),
                _ => unmeasured::owned_by(&prior, package),
            };
            Unmeasured {
                all_items: items.is_empty(),
                shard: common.shard.filter(|_| items.is_empty()),
                detail: format!("{detail}, so {package} saved no numbers"),
                package: package.clone(),
                reason,
                items,
                ..Unmeasured::default()
            }
        })
        .collect();
    if !entries.is_empty() {
        report(name, &entries);
        unmeasured::mark_in(root, name, &entries).map_err(|e| e.to_string())?;
        stamp(root, name, common, listed.unwrap_or_default())?;
    }
    match unread {
        Some(e) => Err(format!(
            "cannot read the workspace's bench packages ({e}); marked the {} package(s) baseline {name:?} names",
            entries.len()
        )),
        None if entries.is_empty() => Err(format!(
            "the workspace has no bench package to mark in baseline {name:?}"
        )),
        None => Ok(()),
    }
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
