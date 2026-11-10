use std::path::Path;

use serde_json::Value;

use super::{Ctx, Row, State};

const SUBJECT: &str = "gate";
const RERUN: &str = "cargo soothfast gate";

/// The last gate verdict `gate` recorded, and the triage it left when it failed.
pub(super) fn check(ctx: &Ctx) -> Vec<Row> {
    let text = std::fs::read_to_string(ctx.root.join(".soothfast/gate-status.json")).ok();
    vec![verdict(&ctx.root, text.as_deref())]
}

fn verdict(root: &Path, text: Option<&str>) -> Row {
    let Some(text) = text else {
        return Row::needs(State::Missing, SUBJECT, "no gate run recorded", RERUN);
    };
    let Ok(doc) = serde_json::from_str::<Value>(text) else {
        return Row::needs(
            State::Stale,
            SUBJECT,
            "gate-status.json is unreadable",
            RERUN,
        );
    };
    if doc["passed"].as_bool() == Some(true) {
        return Row::ok(SUBJECT, "last gate passed");
    }
    let failures = doc["failures"].as_u64().unwrap_or(0);
    let triage = triage_files(root);
    match triage.first() {
        Some(first) => Row::needs(
            State::Failed,
            SUBJECT,
            format!("{failures} failure(s); triage: {}", triage.join(", ")),
            format!("cat .soothfast/triage/{first}"),
        ),
        None => Row::needs(
            State::Failed,
            SUBJECT,
            format!("{failures} failure(s); no triage"),
            RERUN,
        ),
    }
}

fn triage_files(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join(".soothfast/triage"))
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::scratch;

    const PASSED: &str = r#"{"passed":true,"failures":0,"unix":1}"#;
    const FAILED: &str = r#"{"passed":false,"failures":2,"unix":1}"#;

    #[test]
    fn a_passing_gate_is_ok() {
        let root = scratch("gate-ok");
        assert_eq!(verdict(&root, Some(PASSED)).state, State::Ok);
    }

    #[test]
    fn no_recorded_run_is_missing() {
        let root = scratch("gate-missing");
        let rows = check(&Ctx {
            root,
            packages: Vec::new(),
        });
        assert_eq!(rows[0].state, State::Missing);
        assert_eq!(rows[0].fix.as_deref(), Some(RERUN));
    }

    #[test]
    fn a_failed_gate_points_at_its_triage_file() {
        let root = scratch("gate-failed");
        let triage = root.join(".soothfast/triage");
        std::fs::create_dir_all(&triage).unwrap();
        std::fs::write(triage.join("b_item.txt"), "").unwrap();
        std::fs::write(triage.join("a_item.txt"), "").unwrap();
        let row = verdict(&root, Some(FAILED));
        assert_eq!(row.state, State::Failed);
        assert_eq!(row.fix.as_deref(), Some("cat .soothfast/triage/a_item.txt"));
        assert!(row.detail.contains("2 failure(s)"), "{}", row.detail);
    }

    #[test]
    fn a_failed_gate_without_triage_reruns() {
        let root = scratch("gate-no-triage");
        let row = verdict(&root, Some(FAILED));
        assert_eq!(row.state, State::Failed);
        assert_eq!(row.fix.as_deref(), Some(RERUN));
    }

    #[test]
    fn a_corrupt_status_file_is_stale() {
        let root = scratch("gate-corrupt");
        assert_eq!(verdict(&root, Some("{")).state, State::Stale);
    }
}
