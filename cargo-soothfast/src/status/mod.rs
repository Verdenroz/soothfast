//! `cargo soothfast status`: what state the repo's soothfast setup is in and
//! the command that addresses each thing needing action. Reads what other
//! commands already left on disk and in git; builds and measures nothing.

use std::path::PathBuf;

mod baselines;
mod docs_lock;
mod gate;
mod init;
mod render;
mod version;

/// How a subject stands. `Ok` rows need nothing; the rest carry a fix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Ok,
    Stale,
    Failed,
    Missing,
}

impl State {
    pub(crate) fn word(self) -> &'static str {
        match self {
            State::Ok => "ok",
            State::Stale => "stale",
            State::Failed => "failed",
            State::Missing => "missing",
        }
    }
}

/// One line of the report.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) state: State,
    pub(crate) subject: String,
    pub(crate) detail: String,
    pub(crate) fix: Option<String>,
}

impl Row {
    pub(crate) fn ok(subject: &str, detail: impl Into<String>) -> Row {
        Row {
            state: State::Ok,
            subject: subject.into(),
            detail: detail.into(),
            fix: None,
        }
    }

    pub(crate) fn needs(
        state: State,
        subject: &str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Row {
        Row {
            state,
            subject: subject.into(),
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

/// What a check reads from.
pub(crate) struct Ctx {
    pub(crate) root: PathBuf,
    /// Packages with a bench target.
    pub(crate) packages: Vec<String>,
}

/// One independent check: a new one is a function added to [`CHECKS`].
type Check = fn(&Ctx) -> Vec<Row>;

const CHECKS: &[Check] = &[
    version::check,
    gate::check,
    baselines::check,
    docs_lock::check,
    init::check,
];

pub fn run(args: &[String]) -> i32 {
    let mut all = false;
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--all" => all = true,
            "--json" => json = true,
            other => return crate::arg_err(&format!("unknown status arg {other:?}")),
        }
    }
    let root = crate::invoke::project_root();
    let packages = crate::workspace::bench_crates("soothfast", Some(&root)).unwrap_or_default();
    let ctx = Ctx { root, packages };
    let rows: Vec<Row> = CHECKS.iter().flat_map(|check| check(&ctx)).collect();
    if json {
        println!("{}", render::json(&rows));
    } else {
        print!("{}", render::text(&rows, all));
    }
    i32::from(rows.iter().any(|r| r.state != State::Ok))
}

#[cfg(test)]
pub(crate) fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("soothfast-status-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
