//! Measuring the reference ref and HEAD interleaved in a merge-base worktree.

use std::collections::BTreeSet;
use std::time::Instant;

use serde_json::Value;

use super::harness::{harness_of, mark_harness};
use super::identical::{
    StoredFor, base_method_differs, bench_text_digests, identical_head, identical_pass,
    identical_pass_cache, recording,
};
use super::resolve::{NewTarget, Resolved, head_from_cache_or, narrowed, newly_measured};
use super::rounds::{combine_rounds, round_duration};
use crate::buildstamp;
use crate::gate_save::HeadRun;
use crate::headrun::{
    self, RECOUNT, Settled, cache_head, head_from_runcache, ref_doc, settle, settled_doc,
    store_head, store_identical,
};
use crate::invoke::{self, CommonArgs, Run};
use crate::itemreuse::{Bin, Reuse};
use crate::workspace;

/// Measure the merge-base with HEAD in a temp worktree, interleaving rounds
/// (head, base, head, base) tango-style so slow environmental drift cancels;
/// per-metric minima across each side's rounds form the comparison values.
/// Interleaving is a timing concern: the deterministic gating counters
/// (perfcnt/callgrind) don't drift, so the second round of each side skips
/// them and re-measures only the timing-sensitive backends. Advisory walltime
/// can't fail on that second sample, so it skips the second round.
/// With no such bench target at the merge-base, HEAD alone is measured, as
/// [`newly_measured`] decides, or with `measure_new` false nothing is.
/// `Ok(None)` then means there is nothing to gate. A merge-base already
/// measured under the same conditions is served from the run cache instead
/// of being rebuilt, unless `reuse` is false.
pub(super) fn measure_ref_interleaved(
    common: &CommonArgs,
    refname: &str,
    reuse: bool,
    walltime_advisory: bool,
    measure_new: bool,
) -> Result<Option<Resolved>, String> {
    const TIMING_ONLY: &[&str] = &["--skip-gating-counters"];
    enum Ref {
        /// The merge-base has no bench target; whether HEAD has one.
        NoBenchTarget(bool),
        /// The one pass, whether it carries the gating counters, and the run
        /// stored for the identical binary with its label.
        IdenticalBinaries {
            run: Result<Run, String>,
            counted: bool,
            stored: Option<StoredFor>,
        },
        /// Reference served by binary identity rather than by commit.
        Cached(Value),
        /// The two full rounds, the timing-only pair unless walltime is
        /// advisory, then what HEAD's and the merge-base's runs may store.
        Rounds(
            Box<[Result<Run, String>; 2]>,
            Option<Box<[Result<Run, String>; 2]>>,
            Settled,
            Settled,
        ),
    }

    // Worktree builds go to a persistent sibling of the parent target dir so
    // dependencies compile once per machine, not once per gate run.
    let wt_target = invoke::worktree_target_dir().ok();
    let measure = |dir: Option<&std::path::Path>, extra: &[&str]| -> Result<Run, String> {
        let td = dir.and(wt_target.as_deref());
        let recs = invoke::run_bench_dir(common, extra, dir, td).map_err(|e| e.to_string())?;
        let mut run = invoke::collect(&recs);
        run.build = Some(buildstamp::capture(
            common.codegen_units_stamp().as_deref(),
            dir,
        ));
        Ok(run)
    };

    let base_sha = invoke::git(&["merge-base", "HEAD", refname])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let head_stamp = buildstamp::capture(common.codegen_units_stamp().as_deref(), None);
    // Naming HEAD's binary costs a no-run build plus an objdump, so it is
    // taken once and threaded to every lookup and store below.
    let (head_dig, runs, head_exe) = headrun::probe("gate", common, &head_stamp);
    // HEAD alone, where the reference is already measured: items whose code
    // a stored measurement covers are spliced and the rest are measured.
    let measure_head_with =
        |exe: Option<&std::path::Path>, unplanned: &[&str]| -> Result<(Run, bool), String> {
            Reuse::measure_head(
                "gate",
                common,
                &runs,
                exe.map(|exe| Bin::head(exe, head_dig.as_deref())),
                recording(),
                unplanned,
                || buildstamp::capture(common.codegen_units_stamp().as_deref(), None),
                |args| measure(None, args),
            )
        };
    let measure_head = |exe: Option<&std::path::Path>| -> Result<Run, String> {
        measure_head_with(exe, &[]).map(|(run, _)| run)
    };
    // Stored regardless of `reuse`: a fresh measurement is worth keeping
    // even for an invocation that declined to trust an existing one.
    let base_known = !base_sha.is_empty();

    if let Some((_, doc)) = runs.reusable_reference(reuse, &base_sha) {
        println!("gate: reusing the measured merge-base {base_sha}");
        let (head, settled, source) = head_from_cache_or(&runs, head_dig.as_deref(), || {
            let head = measure_head(head_exe.as_deref())?;
            let settled = settle("gate", "HEAD's run", &head, || measure(None, RECOUNT).ok());
            Ok((head, settled))
        })?;
        cache_head(&runs, &head, head_dig.as_deref(), &settled);
        return Ok(Some(Resolved {
            harness: harness_of(&doc),
            reference: doc,
            current: head,
            head: source,
            exe: head_exe.clone(),
        }));
    }

    println!("gate: measuring merge-base of {refname} in worktree (interleaved rounds)");
    let mut reused: BTreeSet<String> = BTreeSet::new();
    let pkg = common.pkg.clone();
    let target = common.target.clone().unwrap_or_else(|| "soothfast".into());
    let outcome = invoke::with_merge_base_worktree(refname, |wt| {
        // Only checkable when a package was named; without `-p` cargo picks
        // the default member and there is nothing to look up.
        if let Some(pkg) = &pkg
            && !workspace::has_bench_target(pkg, &target, Some(wt)).map_err(|e| e.to_string())?
        {
            let at_head = measure_new
                && workspace::has_bench_target(pkg, &target, None).map_err(|e| e.to_string())?;
            let none = invoke::HarnessSync::default();
            return Ok((Ref::NoBenchTarget(at_head), None, none));
        }
        // A local .cargo/config.toml (untracked, e.g. a linker pin) wouldn't
        // otherwise reach the worktree and would read as a false mismatch.
        invoke::sync_untracked_cargo_config(wt)?;
        // Both bench binaries must embed the same soothfast harness: a
        // measurement-protocol change between the two locked versions would
        // otherwise be reported as a regression in the measured project.
        // Pinning must precede the base build the binary comparison does.
        let harness = invoke::sync_harness_versions(wt)?;
        if let Some(target) = &wt_target {
            crate::seed::seed_worktree(wt, target);
        }
        let digests = bench_text_digests(common, wt, wt_target.as_deref());
        // Callgrind counts differ by method, and a path dependency on the
        // harness is not pinned, so the sides can disagree.
        let head_cg = runs.env().filter(|e| e.gating_backend == "callgrind");
        let base_env = head_cg
            .zip(digests.as_ref())
            .filter(|(_, d)| d.head != d.base)
            .map(|(_, d)| invoke::host_env(&d.base_exe, common.backend.as_deref()));
        let harness = match head_cg
            .and_then(|head| base_method_differs(head, digests.is_some(), base_env.as_ref()))
        {
            Some(base) => harness.with_method(&base, head_cg.map_or("", |h| h.method())),
            None => harness,
        };
        // A run measured from byte-identical machine code under the same
        // conditions is that binary's measurement, whatever commit built it.
        let by_binary = match (reuse, &digests) {
            (true, Some(d)) => runs.load_reference(&d.base, &base_sha),
            _ => None,
        }
        .map(|doc| narrowed(doc, common.shard));

        if let Some(d) = digests.as_ref().filter(|d| d.head == d.base) {
            println!(
                "gate: bench binaries identical (code and data match) — no measurable change possible"
            );
            // Loaded only when the cheap pass can use it.
            let load = || {
                let expected = reuse.then(|| headrun::expected_ids(common, &d.head_exe))??;
                let label = format!("binary {}", d.head);
                let doc = runs.load_covering(&d.head, &label, &expected)?;
                Some(Box::new((doc, label, expected)))
            };
            let early = if recording() { load() } else { None };
            let pass = identical_pass(
                reuse,
                early.is_some(),
                recording(),
                common.backend.as_deref(),
            );
            let exe = pass.plan.then_some(d.head_exe.as_path());
            let mut outcome = measure_head_with(exe, pass.args)
                .map(|(run, planned)| (run, pass.counted(planned)));
            let covering = match &outcome {
                Ok((_, false)) => early.or_else(load),
                _ => None,
            };
            if let Ok((run, false)) = &outcome
                && headrun::uncounted_complexity(run, covering.as_ref().map(|c| &c.0))
            {
                println!(
                    "gate: a complexity claim has no counted verdict stored for this binary; measuring with the gating counters"
                );
                outcome = measure_head_with(exe, &[]).map(|(run, _)| (run, true));
            }
            let counted = outcome.as_ref().is_ok_and(|(_, counted)| *counted);
            let run = outcome.map(|(run, _)| run);
            // Head shares that machine code, so a run of it is its
            // measurement too, and the next commit can reuse it from HEAD.
            let stored = covering.as_ref().map(|c| &c.0);
            if let Ok(pass) = &run
                && let Some(doc) = identical_pass_cache(stored, pass, counted, common.shard)
            {
                let settled = if counted {
                    settle("gate", "HEAD's run", pass, || measure(None, RECOUNT).ok())
                } else {
                    Settled::AsMeasured
                };
                match settled_doc(doc, &settled) {
                    Some(doc) if counted => store_head(&runs, doc, pass, &[head_dig.as_deref()]),
                    Some(doc) => store_identical(&runs, doc, pass, &[head_dig.as_deref()]),
                    None => {}
                }
            }
            let stored = covering.filter(|_| !counted);
            let identical = Ref::IdenticalBinaries {
                run,
                counted,
                stored,
            };
            return Ok((identical, digests, harness));
        }
        if let Some(doc) = by_binary {
            println!("gate: reusing a run measured from the same merge-base binary");
            return Ok((Ref::Cached(doc), digests, harness));
        }
        if !reuse {
            println!("gate: per-item reuse off (--no-reuse-base); measuring in full");
        }
        // A merge-base that counts another way would put its numbers in the
        // store under HEAD's method, so it neither reuses nor records.
        let same_method = harness.method().is_none();
        let item_reuse = digests
            .as_ref()
            .filter(|_| reuse && same_method)
            .and_then(|d| {
                let head = Bin::head(&d.head_exe, Some(&d.head));
                let base = Bin {
                    exe: &d.base_exe,
                    digest: Some(&d.base),
                    dir: Some(wt),
                    target_dir: wt_target.as_deref(),
                    env: base_env.as_ref().and_then(|e| e.as_ref().ok()),
                };
                Reuse::plan(
                    "gate",
                    common,
                    &runs,
                    &[head, base],
                    recording(),
                    "measuring in full",
                )
            });
        let measure_side = |side: usize, extra: &[&str]| -> Result<Run, String> {
            let dir = (side == 1).then_some(wt);
            let Some(reuse) = &item_reuse else {
                return measure(dir, extra);
            };
            let stamp = || buildstamp::capture(common.codegen_units_stamp().as_deref(), dir);
            reuse.measure_side(side, extra, stamp, |args| measure(dir, args))
        };
        let total = if walltime_advisory { 2 } else { 4 };
        let timed = |n: usize, side: usize, extra: &[&str]| {
            let start = Instant::now();
            let run = measure_side(side, extra);
            let label = ["head", "base"][side];
            println!(
                "gate: round {n}/{total} ({label}) done in {}",
                round_duration(start.elapsed())
            );
            run
        };
        let mut rounds = [timed(1, 0, &[]), timed(2, 1, &[])];
        let timing_rounds = (!walltime_advisory)
            .then(|| Box::new([timed(3, 0, TIMING_ONLY), timed(4, 1, TIMING_ONLY)]));
        if let Some(reuse) = &item_reuse {
            for (side, round) in rounds.iter_mut().enumerate() {
                if let Ok(run) = round {
                    reuse.finish(side, run);
                }
            }
            reused = reuse.reused_ids(1).cloned().collect();
        }
        // Read again here, where the merge-base worktree still exists; the
        // comparison itself uses the first reading either way.
        let (head_settled, base_settled) = match (&rounds[0], &rounds[1]) {
            (Ok(head), Ok(base)) => (
                settle("gate", "HEAD's run", head, || measure(None, RECOUNT).ok()),
                settle("gate", "the merge-base run", base, || {
                    measure(Some(wt), RECOUNT).ok()
                }),
            ),
            _ => (Settled::Refused, Settled::Refused),
        };
        Ok((
            Ref::Rounds(Box::new(rounds), timing_rounds, head_settled, base_settled),
            digests,
            harness,
        ))
    })?;
    let (outcome, digests, harness) = outcome;

    let sides = match outcome {
        Ref::NoBenchTarget(_) if !measure_new => return Ok(None),
        Ref::NoBenchTarget(at_head) => {
            println!("gate: no bench target at {refname}");
            let exe = head_exe.clone();
            let head = match (at_head, &head_exe) {
                (false, _) => NewTarget::Absent,
                (true, Some(exe))
                    if invoke::list_items(exe, None).is_some_and(|l| l.is_empty()) =>
                {
                    NewTarget::Empty
                }
                (true, _) => NewTarget::Registers,
            };
            let resolved = newly_measured(head, || {
                let cached = (reuse && !recording())
                    .then(|| head_from_runcache(&runs, head_dig.as_deref()))
                    .flatten();
                if let Some((head, from)) = cached {
                    println!("gate: reusing HEAD's own run, measured from {from}");
                    return Ok(head);
                }
                let head = measure_head(head_exe.as_deref().filter(|_| reuse))?;
                let settled = match runs.env() {
                    Some(_) => settle("gate", "HEAD's run", &head, || measure(None, RECOUNT).ok()),
                    None => Settled::AsMeasured,
                };
                cache_head(&runs, &head, head_dig.as_deref(), &settled);
                Ok(head)
            })?;
            return Ok(resolved.map(|r| Resolved { exe, ..r }));
        }
        // One cheap pass serves as both sides: real items and assertions,
        // zero deltas by construction.
        Ref::IdenticalBinaries {
            run,
            counted,
            stored,
        } => {
            let pass = run?;
            let (head, source) = identical_head(pass, counted, stored);
            return Ok(Some(Resolved {
                reference: ref_doc(&head),
                current: head,
                head: source,
                harness,
                exe: head_exe.clone(),
            }));
        }
        Ref::Cached(doc) => {
            let (head, settled, source) = head_from_cache_or(&runs, head_dig.as_deref(), || {
                let head = measure_head(head_exe.as_deref())?;
                let settled = settle("gate", "HEAD's run", &head, || measure(None, RECOUNT).ok());
                Ok((head, settled))
            })?;
            cache_head(&runs, &head, head_dig.as_deref(), &settled);
            return Ok(Some(Resolved {
                reference: doc,
                current: head,
                head: source,
                harness,
                exe: head_exe.clone(),
            }));
        }
        Ref::Rounds(rounds, timing_rounds, head_settled, base_settled) => {
            let [head1, base1] = *rounds;
            let (base, head) = match timing_rounds {
                Some(timing) => {
                    let [head2, base2] = *timing;
                    (combine_rounds(base1?, base2), combine_rounds(head1?, head2))
                }
                None => (base1?, head1?),
            };
            (base, head, head_settled, base_settled)
        }
    };
    let (base, head, head_settled, base_settled) = sides;
    let mut reference = ref_doc(&base);
    for id in &reused {
        reference["items"][id.as_str()]["reused_from"] = serde_json::json!("item cache");
    }
    mark_harness(&mut reference, &harness);
    if let Some(mut stored) =
        settled_doc(reference.clone(), &base_settled).filter(|_| harness.method().is_none())
    {
        headrun::stamp(&mut stored);
        if base_known {
            runs.store(&base_sha, &stored);
        }
        // Keyed by machine code too, so a later merge-base building the same
        // binary hits even if never gated. Stored regardless of `reuse`.
        if let Some(d) = &digests {
            runs.store(&d.base, &stored);
        }
    }
    if let Some(doc) = settled_doc(ref_doc(&head), &head_settled) {
        let text = digests.as_ref().map(|d| d.head.as_str());
        store_head(&runs, doc, &head, &[head_dig.as_deref(), text]);
    }
    Ok(Some(Resolved {
        reference,
        current: head,
        head: HeadRun::Measured,
        harness,
        exe: head_exe,
    }))
}
