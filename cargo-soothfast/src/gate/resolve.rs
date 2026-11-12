//! Resolving the HEAD run and its reference for one gate invocation.

use serde_json::Value;

use super::args::GateArgs;
use super::measure_ref_interleaved;
use crate::buildstamp;
use crate::gate_save::HeadRun;
use crate::headrun::{Settled, head_from_runcache, ref_doc};
use crate::invoke::{self, CommonArgs, Run};

/// The two sides of one comparison, plus what the reader needs to know
/// about how much the comparison is worth.
pub(super) struct Resolved {
    pub(super) reference: Value,
    pub(super) current: Run,
    pub(super) head: HeadRun,
    pub(super) harness: invoke::HarnessSync,
    /// HEAD's probed bench executable, which lists the items a save names.
    pub(super) exe: Option<std::path::PathBuf>,
}

/// HEAD's run from the run cache, or measured now with `measure`.
pub(super) fn head_from_cache_or(
    runs: &crate::runcache::Runs,
    digest: Option<&str>,
    measure: impl FnOnce() -> Result<(Run, Settled), String>,
) -> Result<(Run, Settled, HeadRun), String> {
    if let Some((head, from)) = head_from_runcache(runs, digest) {
        println!("gate: reusing HEAD's own run, measured from {from}");
        return Ok((head, Settled::AsMeasured, HeadRun::Stored(from)));
    }
    let (head, settled) = measure()?;
    Ok((head, settled, HeadRun::Measured))
}

/// Measure or load both sides of a gate run as a [`Resolved`]. `Ok(None)`
/// only in `--against-ref` mode, when there is nothing to gate: no bench
/// target at the ref, and HEAD has none or registers no item.
pub(super) fn resolve(g: &GateArgs, walltime_advisory: bool) -> Result<Option<Resolved>, String> {
    if g.common.backend.as_deref() == Some("buildcost") {
        let Some(pkg) = g.common.pkg.clone() else {
            return Err("--backend buildcost requires -p PKG".into());
        };
        let head_target = invoke::buildcost_target_dir().map_err(|e| e.to_string())?;
        let current = crate::buildcost::measure(&pkg, &g.matrix, Some(&head_target))?;
        let reference = if let Some(refname) = &g.against_ref {
            let base_target = invoke::buildcost_base_target_dir().map_err(|e| e.to_string())?;
            let base = invoke::with_merge_base_worktree(refname, |wt| {
                invoke::sync_untracked_cargo_config(wt)?;
                crate::buildcost::measure_in(&pkg, &g.matrix, Some(wt), Some(&base_target))
            })?;
            let mut doc = serde_json::json!({ "version": 1 });
            doc["items"] = invoke::run_to_items_value(&base);
            doc
        } else {
            let mut baseline = invoke::load_baseline(&g.baseline)
                .map_err(|e| format!("failed to load baseline: {e}"))?
                .ok_or_else(|| format!("no baseline named {:?}", g.baseline))?;
            // Only buildcost items are in play; drop bench entries from the view.
            if let Some(map) = baseline["items"].as_object_mut() {
                map.retain(|k, _| k.starts_with("buildcost::"));
            }
            baseline
        };
        return Ok(Some(Resolved {
            reference,
            current,
            head: HeadRun::Measured,
            harness: invoke::HarnessSync::default(),
            exe: None,
        }));
    }
    if let Some(refname) = &g.against_ref {
        let new = g.measure_new;
        return measure_ref_interleaved(&g.common, refname, g.reuse_base, walltime_advisory, new);
    }
    let records = invoke::run_bench(&g.common, &[]).map_err(|e| e.to_string())?;
    let mut current = invoke::collect(&records);
    current.build = Some(buildstamp::capture(
        g.common.codegen_units_stamp().as_deref(),
        None,
    ));
    let baseline = invoke::load_baseline(&g.baseline)
        .map_err(|e| format!("failed to load baseline: {e}"))?
        .ok_or_else(|| {
            format!(
                "no baseline named {:?} — create one with `cargo soothfast measure --save-baseline {}`",
                g.baseline, g.baseline
            )
        })?;
    Ok(Some(Resolved {
        reference: baseline,
        current,
        head: HeadRun::Measured,
        harness: invoke::HarnessSync::default(),
        exe: invoke::bench_executable(&g.common, None, None),
    }))
}

/// Print HEAD's checked claims and count the ones that failed.
pub(super) fn assertion_failures(current: &Run, failing_ids: &mut Vec<String>) -> u32 {
    let mut failures = 0u32;
    for a in &current.assertions {
        let verdict = if a.ok { "ok" } else { "FAIL" };
        if !a.ok {
            failures += 1;
            failing_ids.push(a.id.clone());
        }
        println!("{verdict:<5} {} assert {}: {}", a.id, a.kind, a.detail);
    }
    failures
}

/// Record the verdict for `report render` (badge) — best effort, never fatal.
pub(super) fn write_gate_status(failures: u32) {
    let Ok(root) = invoke::workspace_root() else {
        return;
    };
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let doc = serde_json::json!({
        "passed": failures == 0,
        "failures": failures,
        "unix": unix,
    });
    let dir = root.join(".soothfast");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(
        dir.join("gate-status.json"),
        serde_json::to_string_pretty(&doc).unwrap_or_default(),
    );
}

/// `doc` narrowed to the items and claims `shard` owns, when there is one.
pub(super) fn narrowed(mut doc: Value, shard: Option<invoke::Shard>) -> Value {
    let Some(shard) = shard else {
        return doc;
    };
    if let Some(items) = doc.get_mut("items").and_then(Value::as_object_mut) {
        items.retain(|id, _| shard.owns(id));
    }
    if let Some(claims) = doc.get_mut("assertions").and_then(Value::as_array_mut) {
        claims.retain(|a| shard.owns_claim(a["id"].as_str().unwrap_or("")));
    }
    doc
}

/// Why a gate that measured nothing must not pass, unless it is a shard that
/// owns no item now and still checks none it owned is gone.
pub(super) fn nothing_measured(current: &Run, common: &CommonArgs, verb: &str) -> Option<String> {
    if !current.items.is_empty() {
        return None;
    }
    match common.shard {
        Some(shard) if common.filter.is_none() => {
            println!("gate: shard {shard} owns no item now; checking none it owned is gone");
            None
        }
        _ => Some(format!(
            "measured 0 items — check --filter / registry setup; refusing to {verb}"
        )),
    }
}

/// What HEAD has of a crate the merge-base has no bench target for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum NewTarget {
    Absent,
    /// A bench target that registers no item yet.
    Empty,
    Registers,
}

/// HEAD of a crate the merge-base has no bench target for, `measure`d and
/// compared with itself so its own claims decide. `None` when HEAD has no
/// bench target either, or one that registers no item yet, which leaves
/// nothing to gate. A target that registers items but measured none still
/// reaches the gate's zero-items refusal.
pub(super) fn newly_measured(
    head: NewTarget,
    measure: impl FnOnce() -> Result<Run, String>,
) -> Result<Option<Resolved>, String> {
    match head {
        NewTarget::Absent => {
            println!("gate: HEAD has no bench target either");
            Ok(None)
        }
        NewTarget::Empty => {
            println!("gate: HEAD's new bench target registers no items yet");
            Ok(None)
        }
        NewTarget::Registers => {
            println!("gate: HEAD's own claims decide");
            Ok(Some(self_compared(measure()?)))
        }
    }
}

/// `head` as both sides of a comparison, so every delta is zero.
fn self_compared(head: Run) -> Resolved {
    Resolved {
        reference: ref_doc(&head),
        current: head,
        head: HeadRun::Measured,
        harness: invoke::HarnessSync::default(),
        exe: None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{NewTarget, assertion_failures, newly_measured, nothing_measured};
    use crate::gate::args::accept_args;
    use crate::gate::test_support::{gated, new_crate_run, scratch};
    use crate::gate_save::save_head;
    use crate::invoke::{self, CommonArgs, Run};
    use crate::test::fixture::TempDir;

    #[test]
    fn a_shard_that_owns_nothing_is_accepted_as_it_is_gated() {
        let argv: Vec<String> = ["--shard", "3/4"].iter().map(|s| s.to_string()).collect();
        let a = accept_args(&argv).unwrap();
        assert!(nothing_measured(&Run::default(), &a.common, "accept").is_none());
        let refusal = nothing_measured(&Run::default(), &CommonArgs::default(), "accept");
        assert!(refusal.unwrap().ends_with("refusing to accept"));
    }

    /// A new crate gated and saved into `root` through the gate's save path.
    fn save_new_crate(root: &TempDir, holds: bool) -> u32 {
        let resolved = newly_measured(NewTarget::Registers, || Ok(new_crate_run(holds)))
            .unwrap()
            .expect("a crate whose new target registers items is gated");
        let failures = gated(&resolved);
        let common = CommonArgs {
            pkg: Some("pkg".into()),
            ..CommonArgs::default()
        };
        let current = &resolved.current;
        let claims = assertion_failures(current, &mut Vec::new());
        save_head(root, "head", current, &resolved.head, &common, None, claims).unwrap();
        failures
    }

    #[test]
    fn a_passing_new_crate_saves_its_run() {
        let root = scratch("new-crate-pass");
        assert_eq!(save_new_crate(&root, true), 0);
        let saved = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
        assert_eq!(
            saved["items"]["pkg::bt_base_to_htf_index"]["alloc"]["allocs"],
            0
        );
        assert_eq!(saved["assertions"][0]["ok"], true);
    }

    #[test]
    fn a_package_with_no_bench_target_at_head_either_is_not_gated() {
        let measured = newly_measured(NewTarget::Absent, || panic!("nothing to measure"));
        assert!(measured.unwrap().is_none());
    }

    #[test]
    fn a_failing_new_crate_fails_the_gate_and_saves_no_numbers() {
        let root = scratch("new-crate-fail");
        assert_eq!(save_new_crate(&root, false), 1);
        let saved = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
        assert_eq!(saved["items"], json!({}));
        assert_eq!(saved["unmeasured"][0]["reason"], "gate-failed");
        assert_eq!(saved["unmeasured"][0]["package"], "pkg");
        assert!(
            saved["unmeasured"][0]["detail"]
                .as_str()
                .unwrap()
                .contains("no listing")
        );
    }

    #[test]
    fn a_new_bench_target_with_no_items_yet_is_not_gated() {
        let measured = newly_measured(NewTarget::Empty, || panic!("nothing to measure"));
        assert!(measured.unwrap().is_none());
    }

    #[test]
    fn a_new_target_that_registers_items_but_measured_none_reaches_the_zero_items_refusal() {
        let measured = newly_measured(NewTarget::Registers, || Ok(Run::default()));
        let resolved = measured
            .unwrap()
            .expect("gated, so the zero-items guard refuses it");
        let common = CommonArgs::default();
        assert!(nothing_measured(&resolved.current, &common, "gate").is_some());
    }
}
