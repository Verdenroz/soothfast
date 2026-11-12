//! Harness-pin mismatch handling: verdict text, waiver, and the marks stored with a run.

use serde_json::Value;

use crate::invoke;

/// Put the mismatch in front of the deltas it may have produced: a
/// reference built against another harness measures another protocol, so
/// its numbers are not this change's in either direction.
pub(super) fn harness_note(harness: &invoke::HarnessSync) {
    if !harness.unpinned().is_empty() {
        println!(
            "gate: HARNESS MISMATCH — the merge-base could not be pinned to HEAD's soothfast ({}); \
             deltas below may be the harness, not this change",
            harness.unpinned().join(", ")
        );
    }
    if let Some(method) = harness.method_note() {
        println!("gate: HARNESS MISMATCH — {method}; comparison is not conclusive");
    }
}

/// The reasons a harness mismatch has, for one line.
pub(super) fn harness_reasons(harness: &invoke::HarnessSync) -> String {
    let mut reasons = harness.unpinned().to_vec();
    reasons.extend(harness.method_note());
    reasons.join(", ")
}

/// What the verdict line adds about the mismatch. The PR comment carries
/// only the tail of the output, so the header alone would not reach it.
pub(super) fn harness_verdict(harness: &invoke::HarnessSync, regressed: bool) -> String {
    if !harness.is_mismatched() {
        return String::new();
    }
    let reasons = harness_reasons(harness);
    if regressed {
        format!(
            " — measured against a different harness ({reasons}); \
             pass --allow-harness-change if the harness bump explains it"
        )
    } else {
        format!(
            " — but measured against a different harness ({reasons}), \
             so the comparison is not conclusive"
        )
    }
}

/// Whether a regression measured against an unpinned harness is waived.
/// Only comparison failures qualify: an assertion holds HEAD against its
/// own declared numbers, which the reference harness has no say over.
pub(super) fn harness_waiver(harness: &invoke::HarnessSync, allow: bool, regressions: u32) -> bool {
    regressions > 0 && harness.is_mismatched() && allow
}

/// Leave the mismatch beside the triage reports in `dir`, so a downloaded
/// artifact carries the reason its numbers may not be the code's. Removed
/// when the pin held: a stale file would mislabel a valid comparison.
pub(super) fn triage_harness_note(dir: &std::path::Path, harness: &invoke::HarnessSync) {
    let path = dir.join("harness-mismatch.txt");
    if !harness.is_mismatched() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let _ = std::fs::create_dir_all(dir);
    let reasons: String = harness
        .unpinned()
        .iter()
        .cloned()
        .chain(harness.method_note())
        .map(|r| format!("  {r}\n"))
        .collect();
    let _ = std::fs::write(
        &path,
        format!(
            "harness mismatch\n\n\
             The reference bench does not measure the way HEAD's does (a soothfast version the\n\
             merge-base could not be pinned to, or the callgrind method):\n\n\
             {reasons}\n\
             Deltas in this run may be that harness change rather than the measured code.\n"
        ),
    );
}

/// Where a stored reference remembers that its harness could not be pinned,
/// so a run served from the cache carries the mark of the run that measured
/// it rather than reading as a clean comparison.
const HARNESS_KEY: &str = "harness_unpinned";

/// Where it remembers the callgrind methods of the two sides, `[base, head]`.
const HARNESS_METHOD_KEY: &str = "harness_method";

pub(super) fn mark_harness(doc: &mut Value, harness: &invoke::HarnessSync) {
    if !harness.unpinned().is_empty() {
        doc[HARNESS_KEY] = serde_json::json!(harness.unpinned());
    }
    if let Some(method) = harness.method() {
        doc[HARNESS_METHOD_KEY] = serde_json::json!(method);
    }
}

pub(super) fn harness_of(doc: &Value) -> invoke::HarnessSync {
    let labels = doc[HARNESS_KEY]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let sync = invoke::HarnessSync::from_unpinned(labels);
    match (
        doc[HARNESS_METHOD_KEY][0].as_str(),
        doc[HARNESS_METHOD_KEY][1].as_str(),
    ) {
        (Some(base), Some(head)) => sync.with_method(base, head),
        _ => sync,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{harness_of, harness_verdict, harness_waiver, mark_harness, triage_harness_note};
    use crate::invoke::HarnessSync;

    fn mismatched() -> HarnessSync {
        HarnessSync::from_unpinned(vec!["soothfast 0.2.0 -> 0.3.1".into()])
    }

    #[test]
    fn a_regression_on_a_mismatched_harness_names_the_flag_that_waives_it() {
        let text = harness_verdict(&mismatched(), true);
        assert!(text.contains("--allow-harness-change"));
        assert!(text.contains("soothfast 0.2.0 -> 0.3.1"));
    }

    #[test]
    fn a_clean_run_on_a_mismatched_harness_still_says_so() {
        let text = harness_verdict(&mismatched(), false);
        assert!(text.contains("different harness"));
        assert!(text.contains("soothfast 0.2.0 -> 0.3.1"));
        assert!(!text.contains("--allow-harness-change"));
        assert!(harness_verdict(&HarnessSync::default(), false).is_empty());
    }

    #[test]
    fn the_flag_waives_a_regression_and_nothing_else() {
        assert!(harness_waiver(&mismatched(), true, 1));
        assert!(!harness_waiver(&mismatched(), false, 1));
        // A pinned reference means a regression is the code's, flag or not.
        assert!(!harness_waiver(&HarnessSync::default(), true, 1));
        // Nothing regressed, so there is nothing to waive.
        assert!(!harness_waiver(&mismatched(), true, 0));
    }

    #[test]
    fn a_reference_served_from_the_cache_keeps_its_mismatch() {
        let mut doc = json!({ "version": 1, "items": {} });
        mark_harness(&mut doc, &mismatched());
        assert_eq!(harness_of(&doc), mismatched());
        let clean = json!({ "version": 1, "items": {} });
        assert_eq!(harness_of(&clean), HarnessSync::default());
    }

    #[test]
    fn a_stored_reference_is_only_marked_when_the_pin_missed() {
        let mut doc = json!({ "version": 1, "items": {} });
        mark_harness(&mut doc, &HarnessSync::default());
        assert_eq!(doc, json!({ "version": 1, "items": {} }));
    }

    #[test]
    fn the_triage_artifact_carries_the_mismatch_and_drops_it_again() {
        let dir = std::env::temp_dir().join("soothfast-test-triage-harness");
        let _ = std::fs::remove_dir_all(&dir);
        triage_harness_note(&dir, &mismatched());
        let note = std::fs::read_to_string(dir.join("harness-mismatch.txt")).unwrap();
        assert!(note.contains("soothfast 0.2.0 -> 0.3.1"));
        triage_harness_note(&dir, &HarnessSync::default());
        assert!(!dir.join("harness-mismatch.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
