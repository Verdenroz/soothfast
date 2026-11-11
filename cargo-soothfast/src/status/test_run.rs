use serde_json::Value;

use super::{Ctx, Row, State};
use crate::test::last;

const SUBJECT: &str = "test";
const RERUN: &str = "cargo soothfast test";

/// The last `test` run `test` recorded, against the tree on disk now.
pub(super) fn check(ctx: &Ctx) -> Vec<Row> {
    let text = std::fs::read_to_string(ctx.root.join(last::FILE)).ok();
    vec![verdict(
        text.as_deref(),
        last::snapshot(&ctx.root).as_deref(),
    )]
}

fn verdict(text: Option<&str>, now: Option<&str>) -> Row {
    let Some(text) = text else {
        return Row::needs(State::Missing, SUBJECT, "no test run recorded", RERUN);
    };
    let Ok(doc) = serde_json::from_str::<Value>(text) else {
        return Row::needs(
            State::Stale,
            SUBJECT,
            "test-status.json is unreadable",
            RERUN,
        );
    };
    if doc["passed"].as_bool() != Some(true) {
        let failures = doc["failures"].as_u64().unwrap_or(0);
        return Row::needs(
            State::Failed,
            SUBJECT,
            format!("last run had {failures} failure(s)"),
            RERUN,
        );
    }
    match (doc["snapshot"].as_str(), now) {
        (Some(then), Some(now)) if then == now => Row::ok(SUBJECT, "last run passed on this tree"),
        _ => Row::needs(
            State::Stale,
            SUBJECT,
            "last run passed on a different tree",
            RERUN,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::scratch;

    const PASSED: &str = r#"{"passed":true,"failures":0,"unix":1,"snapshot":"t:1"}"#;
    const FAILED: &str = r#"{"passed":false,"failures":2,"unix":1,"snapshot":"t:1"}"#;

    #[test]
    fn a_pass_on_the_same_tree_is_ok() {
        assert_eq!(verdict(Some(PASSED), Some("t:1")).state, State::Ok);
    }

    #[test]
    fn a_pass_on_another_tree_is_stale() {
        let row = verdict(Some(PASSED), Some("t:2"));
        assert_eq!(row.state, State::Stale);
        assert_eq!(row.fix.as_deref(), Some(RERUN));
    }

    #[test]
    fn a_pass_with_no_tree_to_compare_is_stale() {
        assert_eq!(verdict(Some(PASSED), None).state, State::Stale);
    }

    #[test]
    fn a_failed_run_fails_whatever_the_tree() {
        let row = verdict(Some(FAILED), Some("t:1"));
        assert_eq!(row.state, State::Failed);
        assert!(row.detail.contains("2 failure(s)"), "{}", row.detail);
    }

    #[test]
    fn no_record_is_missing() {
        let row = verdict(None, Some("t:1"));
        assert_eq!(row.state, State::Missing);
        assert_eq!(row.fix.as_deref(), Some(RERUN));
    }

    #[test]
    fn a_corrupt_record_is_stale() {
        assert_eq!(verdict(Some("{"), Some("t:1")).state, State::Stale);
    }

    #[test]
    fn a_recorded_run_reads_back_ok_until_the_tree_changes() {
        let root = scratch("test-run");
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&root)
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "-q"]);
        std::fs::write(root.join("a.txt"), "1").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "a"]);
        last::record(&root, 0);
        let ctx = Ctx {
            root: root.clone(),
            packages: Vec::new(),
        };
        assert_eq!(check(&ctx)[0].state, State::Ok);
        std::fs::write(root.join("a.txt"), "2").unwrap();
        assert_eq!(check(&ctx)[0].state, State::Stale);
    }
}
