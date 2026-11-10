use soothfast_docs::lockfile::LOCKFILE;

use super::{Ctx, Row, State};
use crate::config::Config;
use crate::invoke;

const SUBJECT: &str = "docs lock";

/// `soothfast.lock` present once markdown binds prose to code.
pub(super) fn check(ctx: &Ctx) -> Vec<Row> {
    let binds = invoke::git_in(
        &ctx.root,
        &["grep", "-l", "--", "<!-- soothfast:bind", "*.md"],
    )
    .is_ok_and(|s| !s.is_empty());
    if !binds {
        return Vec::new();
    }
    if ctx.root.join(LOCKFILE).is_file() {
        return vec![Row::ok(SUBJECT, format!("{LOCKFILE} present"))];
    }
    let fix = match Config::load(&ctx.root)
        .and_then(|c| c.docs())
        .ok()
        .and_then(|c| c.package)
        .and_then(|p| p.into_iter().next())
    {
        Some(package) => format!("cargo soothfast docs check -p {package}"),
        None => "cargo soothfast docs check".into(),
    };
    vec![Row::needs(
        State::Missing,
        SUBJECT,
        format!("markdown carries bind markers but {LOCKFILE} is absent"),
        fix,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::scratch;

    fn repo(tag: &str, markdown: &str) -> Ctx {
        let root = scratch(tag);
        std::fs::write(root.join("README.md"), markdown).unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
                .args(args)
                .current_dir(&root)
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "-q"]);
        git(&["add", "README.md"]);
        Ctx {
            root,
            packages: Vec::new(),
        }
    }

    #[test]
    fn a_bound_doc_with_a_lock_is_ok() {
        let ctx = repo("docs-ok", "<!-- soothfast:bind a::b -->\n");
        std::fs::write(ctx.root.join(LOCKFILE), "{}").unwrap();
        assert_eq!(check(&ctx)[0].state, State::Ok);
    }

    #[test]
    fn a_bound_doc_without_a_lock_is_missing() {
        let ctx = repo("docs-missing", "<!-- soothfast:bind a::b -->\n");
        let rows = check(&ctx);
        assert_eq!(rows[0].state, State::Missing);
        assert_eq!(rows[0].fix.as_deref(), Some("cargo soothfast docs check"));
    }

    #[test]
    fn a_repo_without_bind_markers_has_no_row() {
        assert!(check(&repo("docs-none", "# plain\n")).is_empty());
    }
}
