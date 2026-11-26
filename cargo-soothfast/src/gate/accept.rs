//! `gate accept`: records the numbers of failing items as accepted.

use std::collections::{BTreeMap, BTreeSet};

use super::args::{AcceptArgs, GateArgs, ImportArgs, accept_args};
use super::compare::{CompareCtx, FailedMetric, compare};
use super::harness::harness_note;
use super::outcome::{err, partial};
use super::resolve::{Resolved, nothing_measured, resolve};
use crate::config::{self, Group};
use crate::gate_lock;
use crate::invoke;

/// Record a reviewed delta into `soothfast-gate.lock`, so a future `gate`
/// treats it as the new ceiling for that bench and metric instead of a
/// regression — without touching the threshold for anything else. Only
/// records benches that are failing, or (with `--headroom`) close enough to
/// failing to plausibly be part of the same shift; an unearned entry would
/// let the lock file accumulate speculative headroom instead of a reviewed
/// record of what actually shifted. `--only` names outside that set are
/// skipped, not fatal — see the exit-code contract at the end of this
/// function.
pub(super) fn accept_cmd(args: &[String]) -> i32 {
    let mut a = match accept_args(args) {
        Ok(a) => a,
        Err(e) => return err(&e),
    };
    let walltime_advisory = match config::apply(&mut a.common, Group::Gate) {
        Ok(applied) => {
            a.baseline = a.baseline.or(applied.settings.baseline);
            applied.gate.walltime_advisory
        }
        Err(e) => return err(&e),
    };
    accept_parsed(a, walltime_advisory)
}

/// `gate accept` once its arguments and config are read.
fn accept_parsed(a: AcceptArgs, walltime_advisory: bool) -> i32 {
    let Some(justification) = a.justification.clone() else {
        return err("gate accept needs --justification \"...\"");
    };

    let g = GateArgs {
        common: a.common,
        baseline: a.baseline.unwrap_or_else(|| "base".into()),
        ratchet: None,
        against_ref: a.against_ref,
        deps: false,
        allow_gone: false,
        allow_harness_change: false,
        matrix: a.matrix,
        save_baseline: None,
        reuse_base: true,
        measure_new: false,
        import: ImportArgs::default(),
    };
    let Resolved {
        reference,
        current,
        harness,
        ..
    } = match resolve(&g, walltime_advisory) {
        Ok(Some(resolved)) => resolved,
        Ok(None) => {
            println!("gate accept: nothing to compare — nothing to accept");
            return 0;
        }
        Err(e) => return err(&e),
    };
    harness_note(&harness);
    if let Some(refusal) = nothing_measured(&current, &g.common, "accept") {
        return err(&refusal);
    }
    let root = match invoke::workspace_root() {
        Ok(r) => r,
        Err(e) => return err(&format!("failed to resolve workspace root: {e}")),
    };
    let mut entries = match gate_lock::read(&root) {
        Ok(e) => e,
        Err(e) => return err(&format!("failed to read {}: {e}", gate_lock::LOCKFILE)),
    };

    let ctx = CompareCtx {
        deps_mode: false,
        allow_gone: false,
        filter: g.common.filter.clone(),
        shard: g.common.shard,
        pkg: g.common.pkg.clone(),
        buildcost_mode: g.common.backend.as_deref() == Some("buildcost"),
        accepted: entries.clone(),
        headroom_pct: a.headroom,
        walltime_advisory,
    };
    let mut candidates = Vec::new();
    let mut near_miss = Vec::new();
    compare(
        &reference,
        &current,
        "",
        &ctx,
        &mut Vec::new(),
        &mut candidates,
        &mut near_miss,
    );
    candidates.extend(near_miss);

    if candidates.is_empty() && a.only.is_empty() {
        println!("gate accept: nothing is failing — nothing to accept");
        return 0;
    }

    let available_ids: BTreeSet<&str> = candidates.iter().map(|f| f.id.as_str()).collect();
    let (selected_ids, skipped): (BTreeSet<&str>, Vec<&str>) = if a.only.is_empty() {
        (available_ids, Vec::new())
    } else {
        let mut selected = BTreeSet::new();
        let mut skipped = Vec::new();
        for id in &a.only {
            if available_ids.contains(id.as_str()) {
                selected.insert(id.as_str());
            } else {
                skipped.push(id.as_str());
            }
        }
        (selected, skipped)
    };
    for id in &skipped {
        println!("WARN: {id} is not currently failing or headroom-eligible — skipped");
    }

    let mut by_id: BTreeMap<&str, Vec<&FailedMetric>> = BTreeMap::new();
    for m in &candidates {
        if selected_ids.contains(m.id.as_str()) {
            by_id.entry(m.id.as_str()).or_default().push(m);
        }
    }
    if by_id.is_empty() {
        return partial(&format!(
            "none of the named benches ({}) are currently failing or headroom-eligible",
            a.only.join(", ")
        ));
    }

    let accepted_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for (id, metrics) in &by_id {
        let entry = entries.entry(id.to_string()).or_default();
        entry.base_fingerprint = reference["items"][*id]["fingerprint"]
            .as_str()
            .unwrap_or("")
            .to_string();
        entry.head_fingerprint = current
            .items
            .get(*id)
            .map(|m| m.fingerprint.clone())
            .unwrap_or_default();
        for m in metrics {
            entry.metrics.insert(m.metric.clone(), m.new_value);
        }
        entry.justifications.push(gate_lock::Justification {
            text: justification.clone(),
            accepted_unix,
        });
        println!(
            "gate accept: {id} — {} metric(s) accepted ({justification:?})",
            metrics.len()
        );
    }

    // Written before the exit code, so a partial `--only` match (exit 3
    // below) still leaves the lock file usable for the next CI step.
    if let Err(e) = gate_lock::write(&root, &entries) {
        return err(&format!("failed to write {}: {e}", gate_lock::LOCKFILE));
    }
    println!(
        "gate accept: recorded {} bench(es) in {}",
        by_id.len(),
        gate_lock::LOCKFILE
    );
    if skipped.is_empty() { 0 } else { 3 }
}
