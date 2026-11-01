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
    if let Some(shard) = common.shard {
        baseline_merge::check_shard_in(root, name, shard)?;
    }
    if failures > 0 && common.backend.as_deref() == Some("buildcost") {
        eprintln!("soothfast: refusing to save baseline {name:?}: gate failed");
        return Ok(());
    }
    if failures > 0 {
        eprintln!("soothfast: saving no numbers in baseline {name:?}: gate failed");
        let ids: Vec<String> = match listed {
            Some(ids) => ids.to_vec(),
            None => current.items.keys().cloned().collect(),
        };
        mark_failed(root, name, common, &ids, "the gate failed")?;
    } else {
        let (run, entries) = baseline_material(current, source);
        let scope = invoke::SaveScope::of(common);
        let path = invoke::save_baseline_in(root, name, &run, scope).map_err(|e| e.to_string())?;
        unmeasured::append_in(root, name, &entries).map_err(|e| e.to_string())?;
        println!("baseline saved: {}", path.display());
        report(name, &entries);
    }
    stamp(root, name, common, listed)
}

/// `--save-baseline NAME` for a gate that errored before it had a run.
pub fn save_errored(
    root: &Path,
    name: &str,
    common: &CommonArgs,
    listed: Option<&[String]>,
    why: &str,
) -> Result<(), String> {
    if let Some(shard) = common.shard {
        baseline_merge::check_shard_in(root, name, shard)?;
    }
    let ids = listed.unwrap_or_default();
    mark_failed(root, name, common, ids, &format!("the gate errored: {why}"))?;
    stamp(root, name, common, listed)
}

fn stamp(
    root: &Path,
    name: &str,
    common: &CommonArgs,
    listed: Option<&[String]>,
) -> Result<(), String> {
    match common.shard {
        Some(shard) => {
            baseline_merge::stamp_shard_in(root, name, shard, listed.unwrap_or_default())
                .map_err(|e| e.to_string())
        }
        None => Ok(()),
    }
}

/// Replace the package's numbers in baseline `name` with one `gate-failed`
/// entry naming the items of `ids` this gate owns.
fn mark_failed(
    root: &Path,
    name: &str,
    common: &CommonArgs,
    ids: &[String],
    detail: &str,
) -> Result<(), String> {
    let Some(pkg) = common.pkg.as_ref().map(|p| p.replace('-', "_")) else {
        return Ok(());
    };
    let items: Vec<String> = ids
        .iter()
        .filter(|id| invoke::id_pkg(id) == pkg && common.shard.is_none_or(|s| s.owns(id)))
        .cloned()
        .collect();
    let entry = Unmeasured {
        package: pkg.clone(),
        reason: Reason::GateFailed,
        detail: format!("{detail}, so {pkg} saved no numbers"),
        items,
        metrics: Vec::new(),
    };
    report(name, std::slice::from_ref(&entry));
    unmeasured::mark_in(root, name, &[pkg], &[entry]).map_err(|e| e.to_string())
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
fn baseline_material(current: &Run, source: &HeadRun) -> (Run, Vec<Unmeasured>) {
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
        entries.extend(unmeasured::by_package(
            Reason::ReusedWalltime,
            &detail,
            &["walltime"],
            &reused_wall,
        ));
    }
    if *source == HeadRun::Uncounted {
        entries.extend(unmeasured::by_package(
            Reason::IdenticalBinaries,
            "bench binaries identical to the merge-base, and no stored run of them held the gating counters",
            &["perfcnt", "callgrind"],
            current.items.keys(),
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
