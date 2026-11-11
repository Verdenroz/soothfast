use super::{Ctx, Row, State};
use crate::init::{self, Drift};

const SUBJECT: &str = "init";
const FIX: &str = "cargo soothfast init";
const WORKFLOW: &str = ".github/workflows/soothfast.yml";

/// The files `init` writes against what it would write now, computed offline.
pub(super) fn check(ctx: &Ctx) -> Vec<Row> {
    vec![verdict(init::drift(&ctx.root))]
}

fn verdict(drift: Result<Drift, String>) -> Row {
    let drift = match drift {
        Ok(drift) => drift,
        Err(e) => {
            let first = e.lines().next().unwrap_or_default();
            return Row::needs(State::Stale, SUBJECT, first, FIX);
        }
    };
    if drift.missing.iter().any(|p| p == WORKFLOW) {
        return Row::needs(
            State::Missing,
            SUBJECT,
            format!("{WORKFLOW} is absent"),
            FIX,
        );
    }
    let files: Vec<String> = drift.missing.into_iter().chain(drift.differs).collect();
    if files.is_empty() {
        return Row::ok(SUBJECT, "files match what init writes");
    }
    Row::needs(
        State::Stale,
        SUBJECT,
        format!("drifted from init: {}", files.join(", ")),
        FIX,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::scratch;

    fn drift(missing: &[&str], differs: &[&str]) -> Result<Drift, String> {
        let own = |l: &[&str]| l.iter().map(|s| s.to_string()).collect();
        Ok(Drift {
            missing: own(missing),
            differs: own(differs),
        })
    }

    #[test]
    fn nothing_drifted_is_ok() {
        assert_eq!(verdict(drift(&[], &[])).state, State::Ok);
    }

    #[test]
    fn a_differing_file_is_stale_and_named() {
        let row = verdict(drift(&[], &[WORKFLOW, "soothfast.toml"]));
        assert_eq!(row.state, State::Stale);
        assert!(row.detail.contains("soothfast.toml"), "{}", row.detail);
        assert_eq!(row.fix.as_deref(), Some(FIX));
    }

    #[test]
    fn a_missing_gitignore_is_stale() {
        let row = verdict(drift(&[".gitignore"], &[]));
        assert_eq!(row.state, State::Stale);
        assert!(row.detail.contains(".gitignore"), "{}", row.detail);
    }

    #[test]
    fn an_absent_workflow_is_missing() {
        let row = verdict(drift(&[WORKFLOW, "soothfast.toml"], &[]));
        assert_eq!(row.state, State::Missing);
        assert_eq!(row.fix.as_deref(), Some(FIX));
    }

    #[test]
    fn an_unreadable_setup_is_stale_with_its_first_line() {
        let row = verdict(Err("soothfast.toml: bad\nmore".into()));
        assert_eq!(row.state, State::Stale);
        assert_eq!(row.detail, "soothfast.toml: bad");
    }

    #[test]
    fn a_repo_without_a_workflow_is_missing_offline() {
        let root = scratch("init-bare");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "").unwrap();
        let rows = check(&Ctx {
            root,
            packages: Vec::new(),
        });
        assert_eq!(rows[0].state, State::Missing);
    }
}
