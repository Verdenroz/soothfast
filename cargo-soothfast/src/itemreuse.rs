//! Per-item reuse in one callgrind measurement: the items a stored entry
//! covers in every binary measured are spliced in, the rest are measured,
//! and what was measured is kept (see `itemcache`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::buildstamp::BuildStamp;
use crate::codekey::{self, Binary};
use crate::headrun;
use crate::invoke::{self, AssertionOutcome, CommonArgs, Reused, Run};
use crate::itemcache::{Entry, Items};
use crate::linkmap::{self, Extent};
use crate::runcache::{self, Runs};

/// A bench binary and where it was built, which a link map needs.
pub struct Bin<'a> {
    pub exe: &'a Path,
    /// [`headrun::binary_digest`] of `exe`, when it could be read.
    pub digest: Option<&'a str>,
    pub dir: Option<&'a Path>,
    pub target_dir: Option<&'a Path>,
}

impl<'a> Bin<'a> {
    /// HEAD's bench binary, built in the working tree.
    pub fn head(exe: &'a Path, digest: Option<&'a str>) -> Bin<'a> {
        Bin {
            exe,
            digest,
            dir: None,
            target_dir: None,
        }
    }
}

/// What a callgrind measurement may take from the store: per binary, the
/// items whose executed code a stored entry covers there, and the items left
/// to run.
pub struct Reuse {
    items: Items,
    hits: Vec<BTreeMap<String, Entry>>,
    todo: Vec<Vec<String>>,
    measured_on: Value,
    /// Each binary measured, with its link-map extents.
    sides: Vec<(PathBuf, Vec<Extent>)>,
}

impl Reuse {
    /// Plan the measurement of `bins` (HEAD's binary, then the merge-base's
    /// when gating). `None`, with the reason printed where the user can act
    /// on it, when per-item reuse does not apply. A `recording` run reuses
    /// nothing and measures in full, but its plan still stores what it measures.
    pub fn plan(
        cmd: &str,
        common: &CommonArgs,
        runs: &Runs,
        bins: &[Bin],
        recording: bool,
    ) -> Option<Reuse> {
        let env = runs.env().filter(|e| e.gating_backend == "callgrind")?;
        if common.no_item_cache {
            println!("{cmd}: per-item reuse off (--no-item-cache); measuring in full");
            return None;
        }
        let backend = common.backend.as_deref();
        let capable = env.item_reuse
            && bins[1..]
                .iter()
                .all(|bin| invoke::host_env(bin.exe, backend).is_ok_and(|e| e.item_reuse));
        if !capable {
            println!(
                "{cmd}: per-item reuse disabled for this invocation: bench harness does not take --id (harness {}, CLI {})",
                invoke::harness_versions(),
                env!("CARGO_PKG_VERSION")
            );
            return None;
        }
        let sides: Vec<(PathBuf, Vec<Extent>)> = bins
            .iter()
            .map(|b| (b.exe.to_path_buf(), linkmap::extents(cmd, common, b)))
            .collect();
        let items = Items::open(&runs.item_scope()?)?;
        let (hits, todo) = if recording {
            println!(
                "{cmd}: per-item reuse off (SOOTHFAST_RECORD=1); measuring in full and storing the items it measures"
            );
            (
                vec![BTreeMap::new(); bins.len()],
                vec![Vec::new(); bins.len()],
            )
        } else {
            let listed: Vec<BTreeMap<String, String>> = bins
                .iter()
                .map(|bin| invoke::list_items(bin.exe, common.filter.as_deref()))
                .collect::<Option<_>>()?;
            let (hits, misses) = items.reusable(&listed, &sides);
            let todo: Vec<Vec<String>> = listed
                .iter()
                .zip(&hits)
                .map(|(l, hit)| {
                    l.keys()
                        .filter(|id| !hit.contains_key(*id))
                        .cloned()
                        .collect()
                })
                .collect();
            let why = misses.to_string();
            let why = if why.is_empty() {
                why
            } else {
                format!(" ({why})")
            };
            println!("{cmd}: {}{why}", reuse_summary(&listed, &todo));
            (hits, todo)
        };
        Some(Reuse {
            items,
            hits,
            todo,
            measured_on: runcache::measured_on(env),
            sides,
        })
    }

    /// HEAD's run alone: planned for `head` and measured with
    /// [`Reuse::run_alone`], else measured in full with `measure`.
    pub fn measure_head(
        cmd: &str,
        common: &CommonArgs,
        runs: &Runs,
        head: Option<Bin>,
        recording: bool,
        stamp: impl FnOnce() -> BuildStamp,
        measure: impl FnOnce(&[&str]) -> Result<Run, String>,
    ) -> Result<Run, String> {
        match head.and_then(|bin| Reuse::plan(cmd, common, runs, &[bin], recording)) {
            Some(reuse) => reuse.run_alone(stamp, measure),
            None => measure(&[]),
        }
    }

    /// Measure what binary `side` still needs: `measure` gets `extra` plus an
    /// `--id` per item left, and a side with nothing left gets an empty run.
    pub fn measure_side(
        &self,
        side: usize,
        extra: &[&str],
        stamp: impl FnOnce() -> BuildStamp,
        measure: impl FnOnce(&[&str]) -> Result<Run, String>,
    ) -> Result<Run, String> {
        if self.hits[side].is_empty() {
            return measure(extra);
        }
        if self.todo[side].is_empty() {
            return Ok(Run {
                gating_backend: Some("callgrind".into()),
                build: Some(stamp()),
                ..Default::default()
            });
        }
        let ids: Vec<&str> = self.todo[side]
            .iter()
            .flat_map(|id| ["--id", id.as_str()])
            .collect();
        measure(&[extra, &ids].concat())
    }

    /// A lone binary's run: the items left measured and kept, the rest spliced.
    pub fn run_alone(
        &self,
        stamp: impl FnOnce() -> BuildStamp,
        measure: impl FnOnce(&[&str]) -> Result<Run, String>,
    ) -> Result<Run, String> {
        let mut run = self.measure_side(0, &[], stamp, measure)?;
        self.finish(0, &mut run);
        Ok(run)
    }

    /// Keep what `run` measured of binary `side`, then splice the items
    /// reused for it.
    pub fn finish(&self, side: usize, run: &mut Run) {
        self.record(side, run);
        self.splice(side, run);
    }

    /// Ids [`Reuse::finish`] splices into binary `side`'s run.
    pub fn reused_ids(&self, side: usize) -> impl Iterator<Item = &String> {
        self.hits[side].keys()
    }

    /// Put the stored metrics and assertions of every item reused for binary
    /// `side` into `run`.
    fn splice(&self, side: usize, run: &mut Run) {
        for (id, entry) in &self.hits[side] {
            let mut metrics = entry.metrics.clone();
            metrics.reused = Some(Reused {
                from: "item cache".into(),
                metrics: headrun::reused_metrics(&metrics, true),
            });
            run.items.insert(id.clone(), metrics);
            run.assertions.retain(|a| &a.id != id);
            run.assertions.extend(entry.assertions.iter().cloned());
        }
    }

    /// Keep what `run` measured of binary `side` for later invocations.
    /// An item with a failed claim is not kept: a failure is re-measured, as
    /// the run cache never saves a run whose assertions failed.
    fn record(&self, side: usize, run: &Run) {
        if run.executed.is_empty() {
            return;
        }
        let (bin, inputs) = &self.sides[side];
        let functions: BTreeMap<&String, Vec<(String, String)>> = run
            .executed
            .iter()
            .map(|(id, f)| (id, codekey::portable(bin, f)))
            .collect();
        let wanted = codekey::exe_symbols(functions.values().flatten());
        let Some(binary) = Binary::open(bin, &wanted, inputs) else {
            return;
        };
        for (id, functions) in functions {
            let Some(metrics) = run.items.get(id) else {
                continue;
            };
            let Some(functions) = binary.name_addresses(functions) else {
                continue;
            };
            let assertions: Vec<AssertionOutcome> = run
                .assertions
                .iter()
                .filter(|a| &a.id == id)
                .cloned()
                .collect();
            if assertions.iter().any(|a| !a.ok) {
                continue;
            }
            if let Some(entry) = Entry::measured(
                &binary,
                functions,
                metrics.clone(),
                assertions,
                self.measured_on.clone(),
            ) {
                self.items.store(id, entry);
            }
        }
        self.items.prune();
    }
}

/// The plan line: how many items each binary reuses and how many it measures.
fn reuse_summary(listed: &[BTreeMap<String, String>], todo: &[Vec<String>]) -> String {
    let all: BTreeSet<&String> = listed.iter().flat_map(|l| l.keys()).collect();
    if let [only] = todo {
        return format!(
            "{}/{} items run code a stored measurement covers, measuring {}",
            all.len() - only.len(),
            all.len(),
            only.len()
        );
    }
    let sides: Vec<String> = ["head", "base"]
        .iter()
        .zip(listed.iter().zip(todo))
        .map(|(label, (l, t))| format!("{label} {}/{}", l.len() - t.len(), l.len()))
        .collect();
    let measuring: usize = todo.iter().map(Vec::len).sum();
    format!(
        "items a stored measurement covers: {}, measuring {measuring}",
        sides.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(ids: &[&str]) -> BTreeMap<String, String> {
        ids.iter()
            .map(|id| (id.to_string(), "fp".to_string()))
            .collect()
    }

    fn todo(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn a_lone_binary_counts_what_it_still_measures() {
        let line = reuse_summary(&[listed(&["a", "b", "c"])], &[todo(&["c"])]);
        assert_eq!(
            line,
            "2/3 items run code a stored measurement covers, measuring 1"
        );
    }

    #[test]
    fn a_gate_counts_each_side_on_its_own() {
        let both = listed(&["a", "b"]);
        let line = reuse_summary(&[both.clone(), both], &[todo(&["b"]), Vec::new()]);
        assert_eq!(
            line,
            "items a stored measurement covers: head 1/2, base 2/2, measuring 1"
        );
    }
}
