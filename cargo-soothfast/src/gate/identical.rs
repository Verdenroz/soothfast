//! The single-pass path for a merge-base binary identical to HEAD's.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::gate_save::HeadRun;
use crate::headrun::{self, binary_digest, ref_doc};
use crate::invoke::{self, CommonArgs, Run};

/// Whether this gate runs to record HEAD as a reference (`SOOTHFAST_RECORD=1`,
/// set by the action on default-branch pushes) rather than to judge a change.
/// An environment variable, so older CLIs the action installs ignore it.
pub fn recording() -> bool {
    std::env::var("SOOTHFAST_RECORD").is_ok_and(|v| v == "1")
}

/// Runner args for the single pass taken when both sides' binaries match.
/// A pass that `record`s a binary nothing is stored for keeps the gating
/// counters, so it can be stored as that binary's run. A backend named on the
/// command line may be one of the gating counters, which measures nothing once
/// those counters are skipped, so it keeps them.
fn identical_pass_args(backend: Option<&str>, record: bool) -> &'static [&'static str] {
    match backend {
        _ if record => &[],
        Some("perfcnt" | "callgrind") => &[],
        _ => &["--skip-gating-counters"],
    }
}

/// How the identical-binaries branch takes its one pass: planned per item
/// when `plan` (falling back to `args` when no plan applies), else with `args`.
#[derive(Debug, PartialEq)]
pub(super) struct IdenticalPass {
    pub(super) plan: bool,
    pub(super) args: &'static [&'static str],
}

impl IdenticalPass {
    /// Whether the pass carries the gating counters, `planned` saying the plan
    /// was used. Only then is its run a baseline and a stored measurement.
    pub(super) fn counted(&self, planned: bool) -> bool {
        planned || self.args.is_empty()
    }
}

/// A run stored for the binary is carried forward by a plan too, which splices
/// its items and their claims from the item store instead of measuring them.
/// A pass that skips the counters is measured again with them when one of
/// its complexity claims has no counted verdict `stored`.
pub(super) fn identical_pass(
    reuse: bool,
    stored: bool,
    recording: bool,
    backend: Option<&str>,
) -> IdenticalPass {
    IdenticalPass {
        plan: reuse,
        args: identical_pass_args(backend, !stored && recording),
    }
}

/// What the identical-binaries branch keeps as HEAD's run: a pass that carries
/// the gating counters, else the run already stored for the binary. A pass
/// that skipped them must never be stored, or a later gate reusing it compares
/// nothing.
/// Under a shard, the stored run is narrowed to the shard's own ids, since it
/// is kept under the shard's HEAD key.
pub(super) fn identical_pass_cache(
    stored: Option<&Value>,
    pass: &Run,
    counted: bool,
    shard: Option<invoke::Shard>,
) -> Option<Value> {
    if counted {
        return Some(ref_doc(pass));
    }
    let mut doc = stored.cloned()?;
    if let Some(shard) = shard {
        if let Some(items) = doc["items"].as_object_mut() {
            items.retain(|id, _| shard.owns(id));
        }
        if let Some(assertions) = doc["assertions"].as_array_mut() {
            assertions.retain(|a| shard.owns_claim(a["id"].as_str().unwrap_or("")));
        }
        if let Some(executed) = doc["executed"].as_object_mut() {
            executed.retain(|id, _| shard.owns(id));
        }
    }
    Some(doc)
}

/// A run stored for the identical binary, its label, and the ids the pass
/// was asked to measure.
pub(super) type StoredFor = Box<(Value, String, BTreeSet<String>)>;

/// HEAD's run from the identical-binaries pass. A pass without the gating
/// counters takes them from the run stored for the byte-identical binary.
pub(super) fn identical_head(
    pass: Run,
    counted: bool,
    stored: Option<StoredFor>,
) -> (Run, HeadRun) {
    if counted {
        return (pass, HeadRun::Measured);
    }
    let Some((doc, from, expected)) = stored.map(|s| *s) else {
        return (pass, HeadRun::Uncounted);
    };
    match headrun::merge_stored(&doc, &from, pass, &expected) {
        Ok(run) => {
            println!("gate: counters for the identical binaries read from its stored run ({from})");
            (run, HeadRun::Measured)
        }
        Err(pass) => (*pass, HeadRun::Uncounted),
    }
}

/// The callgrind method the merge-base counts with, when it is not HEAD's.
/// `unknown` when it cannot be told: a comparison nobody could check is
/// inconclusive, not matched. `base` is `None` for identical binaries.
pub(super) fn base_method_differs(
    head: &invoke::HostEnv,
    have_digests: bool,
    base: Option<&Result<invoke::HostEnv, String>>,
) -> Option<String> {
    if !have_digests {
        return Some("unknown".into());
    }
    match base? {
        Ok(base) if base.gating_backend != "callgrind" => None,
        Ok(base) => (base.method() != head.method()).then(|| base.method().to_string()),
        Err(_) => Some("unknown".into()),
    }
}

/// The head and merge-base bench binaries, with digests of their machine code.
pub(super) struct Binaries {
    pub(super) head: String,
    pub(super) base: String,
    pub(super) head_exe: std::path::PathBuf,
    pub(super) base_exe: std::path::PathBuf,
}

/// Conservative: any build or extraction failure yields `None`, so the gate
/// falls through to a real measurement.
pub(super) fn bench_text_digests(
    common: &CommonArgs,
    wt: &std::path::Path,
    wt_target: Option<&std::path::Path>,
) -> Option<Binaries> {
    let (Some(head), Some(base)) = (
        invoke::bench_executable(common, None, None),
        invoke::bench_executable(common, Some(wt), wt_target),
    ) else {
        return None;
    };
    // One path for both sides means extraction resolved head's own binary
    // twice; treat it as failed and fall through to a real measurement.
    if head == base {
        return None;
    }
    Some(Binaries {
        head: binary_digest(&head)?,
        base: binary_digest(&base)?,
        head_exe: head,
        base_exe: base,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        base_method_differs, identical_head, identical_pass, identical_pass_args,
        identical_pass_cache,
    };

    use crate::gate::test_support::run_with;
    use crate::invoke::{self, ItemMetrics, Run};

    #[test]
    fn a_named_gating_backend_keeps_its_counters_on_the_identical_pass() {
        assert!(identical_pass_args(Some("perfcnt"), false).is_empty());
        assert!(identical_pass_args(Some("callgrind"), false).is_empty());
    }

    #[test]
    fn the_identical_pass_skips_counters_otherwise() {
        assert_eq!(identical_pass_args(None, false), ["--skip-gating-counters"]);
        assert_eq!(
            identical_pass_args(Some("walltime"), false),
            ["--skip-gating-counters"]
        );
    }

    #[test]
    fn only_a_counted_identical_pass_is_stored_afresh() {
        let pass = Run::default();
        assert!(identical_pass_cache(None, &pass, false, None).is_none());
        assert!(identical_pass_cache(None, &pass, true, None).is_some());
    }

    #[test]
    fn a_stored_run_is_kept_when_the_pass_is_not_counted() {
        let stored = json!({ "version": 1, "items": {} });
        assert_eq!(
            identical_pass_cache(Some(&stored), &Run::default(), false, None),
            Some(stored)
        );
    }

    #[test]
    fn a_shard_keeps_only_its_own_ids_of_the_identical_binary_s_stored_run() {
        let shard = invoke::Shard { index: 1, count: 2 };
        let ids: Vec<String> = (0..8).map(|n| format!("pkg::f{n}")).collect();
        let mut stored = json!({ "version": 1, "items": {} });
        stored["assertions"] = json!([]);
        for id in &ids {
            stored["items"][id] = json!({ "callgrind": { "ir": 1 } });
            stored["executed"][id] = json!([]);
            let verdict = json!({ "id": id, "kind": "alloc", "ok": true, "detail": "" });
            stored["assertions"].as_array_mut().unwrap().push(verdict);
        }
        let doc = identical_pass_cache(Some(&stored), &Run::default(), false, Some(shard)).unwrap();
        let kept: Vec<&String> = doc["items"].as_object().unwrap().keys().collect();
        assert!(!kept.is_empty() && kept.len() < ids.len());
        assert!(kept.iter().all(|id| shard.owns(id)));
        let executed: Vec<&String> = doc["executed"].as_object().unwrap().keys().collect();
        assert_eq!(executed, kept);
        let verdicts = doc["assertions"].as_array().unwrap();
        assert!(
            verdicts
                .iter()
                .all(|a| shard.owns(a["id"].as_str().unwrap()))
        );
        assert_eq!(verdicts.len(), kept.len());
    }

    #[test]
    fn the_identical_branch_plans_whenever_reuse_is_on() {
        for (stored, recording) in [(false, false), (true, false), (false, true), (true, true)] {
            assert!(identical_pass(true, stored, recording, None).plan);
        }
        assert!(!identical_pass(false, false, false, None).plan);
    }

    #[test]
    fn a_plan_that_applies_counts_and_one_that_does_not_takes_the_cheap_pass() {
        let pass = identical_pass(true, false, false, None);
        assert!(pass.counted(true));
        assert!(!pass.counted(false));
        assert_eq!(pass.args, ["--skip-gating-counters"]);
    }

    #[test]
    fn a_stored_run_keeps_the_cheap_fallback_even_when_recording() {
        let pass = identical_pass(false, true, true, None);
        assert_eq!(pass.args, ["--skip-gating-counters"]);
        assert!(!pass.counted(false));
    }

    #[test]
    fn a_recording_fallback_counts_and_is_baseline_material() {
        let pass = identical_pass(false, false, true, Some("walltime"));
        assert!(pass.args.is_empty());
        assert!(pass.counted(false));
    }

    #[test]
    fn a_counted_identical_run_saves_the_baseline() {
        assert!(identical_pass(true, true, false, None).counted(true));
    }

    #[test]
    fn a_recording_identical_pass_keeps_the_counters() {
        assert!(identical_pass_args(None, true).is_empty());
        assert!(identical_pass_args(Some("walltime"), true).is_empty());
    }

    #[test]
    fn an_uncounted_identical_pass_takes_the_identical_binary_s_stored_counters() {
        use crate::gate_save::HeadRun;
        let pass = || {
            run_with(ItemMetrics {
                median_ns: Some(5.0),
                ..ItemMetrics::default()
            })
        };
        let stored = json!({ "items": { "pkg::bt_base_to_htf_index": { "callgrind": { "ir": 7 } } }, "head_format": crate::headrun::HEAD_FORMAT });
        let ids = std::collections::BTreeSet::from(["pkg::bt_base_to_htf_index".to_string()]);
        let stored = Box::new((stored, "binary abc".to_string(), ids));
        let (run, source) = identical_head(pass(), false, Some(stored));
        assert_eq!(source, HeadRun::Measured);
        assert_eq!(run.items["pkg::bt_base_to_htf_index"].ir, Some(7));
        assert_eq!(run.items["pkg::bt_base_to_htf_index"].median_ns, Some(5.0));
        assert_eq!(run.reused.unwrap().from, "binary abc");
        let (_, source) = identical_head(pass(), false, None);
        assert_eq!(source, HeadRun::Uncounted);
    }

    fn callgrind_env(method: Option<&str>) -> invoke::HostEnv {
        invoke::HostEnv {
            gating_backend: "callgrind".into(),
            guest: None,
            callgrind_method: method.map(String::from),
            item_reuse: true,
        }
    }

    #[test]
    fn a_merge_base_that_counts_another_way_or_cannot_say_is_a_mismatch() {
        let head = callgrind_env(Some("one-process"));
        let differs = |have, base| base_method_differs(&head, have, base);
        let two = Ok(callgrind_env(None));
        let one = Ok(callgrind_env(Some("one-process")));
        assert_eq!(differs(true, Some(&two)).as_deref(), Some("two-process"));
        assert_eq!(differs(true, Some(&one)), None);
        assert_eq!(differs(true, None), None);
        assert_eq!(differs(false, None).as_deref(), Some("unknown"));
        assert_eq!(
            differs(true, Some(&Err("no".into()))).as_deref(),
            Some("unknown")
        );
    }

    #[test]
    fn a_merge_base_that_does_not_gate_on_callgrind_is_not_a_method_mismatch() {
        let head = callgrind_env(Some("one-process"));
        let mut walltime = callgrind_env(None);
        walltime.gating_backend = "walltime".into();
        assert_eq!(base_method_differs(&head, true, Some(&Ok(walltime))), None);
    }
}
