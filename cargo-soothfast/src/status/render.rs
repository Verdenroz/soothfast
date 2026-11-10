use serde_json::json;

use super::{Row, State};

/// The rows as lines: state word, subject, detail and, when action is
/// needed, the fix. Ok rows collapse into a count unless `all`.
pub(super) fn text(rows: &[Row], all: bool) -> String {
    let mut out = String::new();
    let mut ok = 0;
    for row in rows {
        let word = row.state.word();
        match &row.fix {
            Some(fix) => out.push_str(&format!(
                "{word:<7} {}  {}  -> {fix}\n",
                row.subject, row.detail
            )),
            None if all => out.push_str(&format!("{word:<7} {}  {}\n", row.subject, row.detail)),
            None => ok += 1,
        }
    }
    if ok > 0 {
        let noun = if ok == 1 { "check" } else { "checks" };
        out.push_str(&format!("{:<7} {ok} {noun}\n", State::Ok.word()));
    }
    out
}

/// The rows as a JSON array of `state`, `subject`, `detail`, `fix`.
pub(super) fn json(rows: &[Row]) -> String {
    let rows: Vec<_> = rows
        .iter()
        .map(|r| {
            json!({
                "state": r.state.word(),
                "subject": r.subject,
                "detail": r.detail,
                "fix": r.fix,
            })
        })
        .collect();
    serde_json::to_string(&rows).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<Row> {
        vec![
            Row::ok("a", "fine"),
            Row::needs(State::Stale, "b", "old", "cargo soothfast fix"),
        ]
    }

    #[test]
    fn ok_rows_collapse_into_a_count() {
        assert_eq!(
            text(&rows(), false),
            "stale   b  old  -> cargo soothfast fix\nok      1 check\n"
        );
    }

    #[test]
    fn all_lists_the_ok_rows() {
        let out = text(&rows(), true);
        assert!(out.starts_with("ok      a  fine\n"), "{out}");
        assert!(!out.contains("check"));
    }

    #[test]
    fn json_carries_stable_fields() {
        let v: serde_json::Value = serde_json::from_str(&json(&rows())).unwrap();
        assert_eq!(v[0]["state"], "ok");
        assert_eq!(v[0]["fix"], serde_json::Value::Null);
        assert_eq!(v[1]["subject"], "b");
        assert_eq!(v[1]["fix"], "cargo soothfast fix");
    }
}
