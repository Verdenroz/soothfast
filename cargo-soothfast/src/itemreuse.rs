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
    /// What `exe` resolves, when the caller already asked it.
    pub env: Option<&'a invoke::HostEnv>,
}

impl<'a> Bin<'a> {
    /// HEAD's bench binary, built in the working tree.
    pub fn head(exe: &'a Path, digest: Option<&'a str>) -> Bin<'a> {
        Bin {
            exe,
            digest,
            dir: None,
            target_dir: None,
            env: None,
        }
    }
}

/// Whether a per-item plan reads link maps, given what HEAD's binary resolves.
pub fn reads_maps(env: Option<&invoke::HostEnv>, common: &CommonArgs) -> bool {
    !common.no_item_cache && env.is_some_and(|e| e.gating_backend == "callgrind" && e.item_reuse)
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
    /// Hashes the executed functions of a binary; [`Binary::open`] outside tests.
    open: fn(&Path, &BTreeSet<String>, &[Extent]) -> Option<Binary>,
}

impl Reuse {
    /// Plan the measurement of `bins` (HEAD's binary, then the merge-base's
    /// when gating). `None`, with the reason printed where the user can act
    /// on it, when per-item reuse does not apply; `otherwise` says what the
    /// caller does then. A `recording` run reuses nothing and measures in
    /// full, but its plan still stores what it measures.
    pub fn plan(
        cmd: &str,
        common: &CommonArgs,
        runs: &Runs,
        bins: &[Bin],
        recording: bool,
        otherwise: &str,
    ) -> Option<Reuse> {
        let env = runs.env().filter(|e| e.gating_backend == "callgrind")?;
        if common.no_item_cache {
            println!("{cmd}: per-item reuse off (--no-item-cache); {otherwise}");
            return None;
        }
        let backend = common.backend.as_deref();
        let capable = env.item_reuse
            && bins[1..].iter().all(|bin| match bin.env {
                Some(env) => env.item_reuse,
                None => invoke::host_env(bin.exe, backend).is_ok_and(|e| e.item_reuse),
            });
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
                .map(|bin| {
                    let mut l = invoke::list_items(bin.exe, common.filter.as_deref())?;
                    if let Some(shard) = common.shard {
                        l.retain(|id, _| shard.owns(id));
                    }
                    Some(l)
                })
                .collect::<Option<_>>()?;
            let (hits, misses) = items.reusable(&listed, &sides);
            let todo = pending(&listed, &hits);
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
            open: Binary::open,
        })
    }

    /// HEAD's run alone: planned for `head` and measured with
    /// [`Reuse::run_alone`], else measured with `unplanned` args (none for a
    /// full measurement). The flag says whether the plan was used.
    #[allow(clippy::too_many_arguments)]
    pub fn measure_head(
        cmd: &str,
        common: &CommonArgs,
        runs: &Runs,
        head: Option<Bin>,
        recording: bool,
        unplanned: &[&str],
        stamp: impl FnOnce() -> BuildStamp,
        measure: impl FnOnce(&[&str]) -> Result<Run, String>,
    ) -> Result<(Run, bool), String> {
        let otherwise = if unplanned.is_empty() {
            "measuring in full"
        } else {
            "taking the cheap pass"
        };
        let plan =
            head.and_then(|bin| Reuse::plan(cmd, common, runs, &[bin], recording, otherwise));
        match plan {
            Some(reuse) => Ok((reuse.run_alone(stamp, measure)?, true)),
            None => Ok((measure(unplanned)?, false)),
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
        let Some(binary) = (self.open)(bin, &wanted, inputs) else {
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

/// Per binary, the listed items no hit covers.
fn pending(
    listed: &[BTreeMap<String, String>],
    hits: &[BTreeMap<String, Entry>],
) -> Vec<Vec<String>> {
    listed
        .iter()
        .zip(hits)
        .map(|(l, hit)| {
            l.keys()
                .filter(|id| !hit.contains_key(*id))
                .cloned()
                .collect()
        })
        .collect()
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
    use crate::invoke::ItemMetrics;

    /// Reads a stand-in binary: one `symbol=hash` per line.
    fn fake_open(bin: &Path, _: &BTreeSet<String>, _: &[Extent]) -> Option<Binary> {
        let text = std::fs::read_to_string(bin).ok()?;
        let hashes = text
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(sym, hash)| (sym.to_string(), hash.parse().unwrap()))
            .collect();
        Some(Binary::with_hashes(hashes))
    }

    /// One lone-binary pass over `ids`, `code` being the stand-in binary,
    /// against the store in `dir`. Returns the ids the mock backend was asked
    /// to measure and the finished run.
    fn lone_pass(dir: &Path, code: &str, ids: &[&str]) -> (Vec<String>, Run) {
        let bin = dir.join("bench");
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(&bin, code).unwrap();
        let listed = [listed(ids)];
        let wanted = todo(ids);
        let items = Items::at(dir.join("scope"));
        let binary = fake_open(&bin, &BTreeSet::new(), &[]).unwrap();
        let (hits, _) = items.reusable_in(&wanted, &items.load(&wanted), &listed, &[binary]);
        let reuse = Reuse {
            todo: pending(&listed, &hits),
            hits,
            items,
            measured_on: Value::Null,
            sides: vec![(bin.clone(), Vec::new())],
            open: fake_open,
        };
        let mut measured = Vec::new();
        let run = reuse
            .run_alone(BuildStamp::fixture, |args| {
                measured = match args {
                    [] => todo(ids),
                    _ => args
                        .iter()
                        .skip(1)
                        .step_by(2)
                        .map(|a| a.to_string())
                        .collect(),
                };
                let item = |id: &String| {
                    let metrics = ItemMetrics {
                        fingerprint: "fp".into(),
                        ir: Some(7),
                        ..Default::default()
                    };
                    (id.clone(), metrics)
                };
                let called = |id: &String| {
                    let function = (bin.display().to_string(), format!("f_{id}"));
                    (id.clone(), vec![function])
                };
                Ok(Run {
                    items: measured.iter().map(item).collect(),
                    executed: measured.iter().map(called).collect(),
                    ..Default::default()
                })
            })
            .expect("mock backend");
        (measured, run)
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("soothfast-reuse-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

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

    #[test]
    fn identical_binaries_with_an_empty_store_measure_and_store_every_item() {
        let dir = scratch("empty");
        let (measured, run) = lone_pass(&dir, "f_a=1\nf_b=1\n", &["a", "b"]);
        assert_eq!(measured, ["a", "b"]);
        assert_eq!(run.items.len(), 2);
        let stored = Items::at(dir.join("scope")).load(&todo(&["a", "b"]));
        assert!(stored.wanted().contains("f_a") && stored.wanted().contains("f_b"));
    }

    #[test]
    fn a_second_identical_run_measures_nothing_and_still_has_every_item() {
        let dir = scratch("repeat");
        lone_pass(&dir, "f_a=1\nf_b=1\n", &["a", "b"]);
        let (measured, run) = lone_pass(&dir, "f_a=1\nf_b=1\n", &["a", "b"]);
        assert!(measured.is_empty());
        assert_eq!(run.items.len(), 2);
        assert_eq!(run.items["a"].ir, Some(7));
        assert!(run.items["a"].reused.is_some());
    }

    #[test]
    fn a_following_run_with_one_changed_item_measures_only_that_item() {
        let dir = scratch("changed");
        lone_pass(&dir, "f_a=1\nf_b=1\n", &["a", "b"]);
        let (measured, run) = lone_pass(&dir, "f_a=1\nf_b=2\n", &["a", "b"]);
        assert_eq!(measured, ["b"]);
        assert!(run.items["a"].reused.is_some());
        assert!(run.items["b"].reused.is_none());
    }
}
