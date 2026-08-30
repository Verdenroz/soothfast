//! `cargo soothfast docs build|serve|gh-deploy` — build the static docs site
//! natively (replaces `mkdocs build`), serve it with rebuild-on-change and
//! live reload (replaces `mkdocs serve`), or publish it to a gh-pages-style
//! branch (replaces `mkdocs gh-deploy`).

use std::path::{Path, PathBuf};

use serde_json::Value;
use soothfast_docs::lockfile;
use soothfast_site::{BuildInput, SiteConfig, build, config, evidence, nav, search, serve};

use crate::invoke;

/// Load config + baseline + lockfile into a ready BuildInput. Re-run per
/// rebuild in serve mode so config/baseline edits are picked up.
fn assemble(
    root: &Path,
    baseline: &str,
    out: Option<&Path>,
    missing_nav_page: nav::Missing,
) -> Result<BuildInput, String> {
    let cfg_path = root.join("soothfast.toml");
    let mut cfg = if cfg_path.exists() {
        let text = std::fs::read_to_string(&cfg_path)
            .map_err(|e| format!("{}: {e}", cfg_path.display()))?;
        config::parse(&text).map_err(|e| format!("soothfast.toml: {e}"))?
    } else {
        SiteConfig::default()
    };
    if let Some(dir) = out {
        cfg.out_dir = dir.to_path_buf();
    }

    let baseline_data = match invoke::load_baseline(baseline) {
        Ok(Some(b)) => b,
        Ok(None) => {
            println!("note: no baseline {baseline:?} — claim chips will render as unmeasured");
            Value::Null
        }
        Err(e) => return Err(format!("cannot load baseline: {e}")),
    };
    // A missing lockfile is fine (no binds yet); a corrupt one is not —
    // rendering every bind as "not locked" would misstate verification.
    let binds = lockfile::read(root)
        .map_err(|e| format!("cannot read soothfast.lock: {e}"))?
        .binds;

    let mut plugins: Vec<Box<dyn soothfast_site::SitePlugin>> =
        vec![Box::new(evidence::Evidence::new(baseline_data, binds))];
    if cfg.search {
        plugins.push(Box::new(search::Search));
    }

    Ok(BuildInput {
        root: root.to_path_buf(),
        config: cfg,
        plugins,
        missing_nav_page,
    })
}

/// A build's non-fatal problems. Warnings that nobody sees are the thing
/// this whole tool exists to avoid, so they go to stderr where a pipe to a
/// log still keeps them.
fn report_warnings(report: &soothfast_site::BuildReport) {
    for w in &report.warnings {
        eprintln!("warning: {w}");
    }
}

/// One-shot static build.
pub fn build_cmd(baseline: &str, out: Option<&Path>) -> i32 {
    let root = match invoke::workspace_root() {
        Ok(r) => r,
        Err(e) => return err(&e.to_string()),
    };
    if !root.join("soothfast.toml").exists() {
        println!("note: no soothfast.toml — building with defaults (auto nav)");
    }
    let input = match assemble(&root, baseline, out, nav::Missing::Fail) {
        Ok(i) => i,
        Err(e) => return err(&e),
    };
    match build(&input) {
        Ok(report) => {
            report_warnings(&report);
            println!(
                "docs build: {} page(s), {} asset(s) → {}",
                report.pages,
                report.assets,
                root.join(&input.config.out_dir).display()
            );
            0
        }
        Err(e) => err(&e),
    }
}

/// Dev server: build into a scratch dir, serve it, rebuild on change.
pub fn serve_cmd(baseline: &str, addr: Option<&str>) -> i32 {
    let root = match invoke::workspace_root() {
        Ok(r) => r,
        Err(e) => return err(&e.to_string()),
    };
    // Never serve (or wipe) the user's real out_dir: dev builds are scratch.
    let serve_dir = root.join(".soothfast").join("serve");

    let input = match assemble(&root, baseline, Some(&serve_dir), nav::Missing::Warn) {
        Ok(i) => i,
        Err(e) => return err(&e),
    };
    match build(&input) {
        Ok(report) => report_warnings(&report),
        Err(e) => return err(&e),
    }

    // Watch sources, config, and any theme overlay — not the output.
    let mut watch = vec![
        root.join(&input.config.docs_dir),
        root.join("soothfast.toml"),
    ];
    if let Some(theme_dir) = &input.config.theme_dir {
        watch.push(root.join(theme_dir));
    }
    let watch: Vec<PathBuf> = watch;

    let server = match serve::Server::bind(addr.unwrap_or("127.0.0.1:8000")) {
        Ok(s) => s,
        Err(e) => return err(&e),
    };
    println!("serving docs at http://{}/ (Ctrl-C to stop)", server.addr());
    println!("watching {} path(s) for changes", watch.len());

    let baseline = baseline.to_string();
    let rebuild_dir = serve_dir.clone();
    let result = server.run(&serve_dir, &watch, move || {
        let input = assemble(&root, &baseline, Some(&rebuild_dir), nav::Missing::Warn)?;
        let report = build(&input)?;
        report_warnings(&report);
        Ok(report)
    });
    match result {
        Ok(()) => 0,
        Err(e) => err(&e),
    }
}

/// Build the site, then publish it wholesale to `branch` on `remote`: an
/// existing branch is reset to its remote tip and its contents replaced
/// (so removed/renamed pages don't linger), a first-ever deploy gets an
/// orphan branch. Pushed with an ordinary (non-force) push — if the local
/// view of `branch` is stale, git rejects it rather than clobbering history
/// we haven't seen.
pub fn gh_deploy_cmd(baseline: &str, branch: &str, remote: &str, message: Option<&str>) -> i32 {
    let root = match invoke::workspace_root() {
        Ok(r) => r,
        Err(e) => return err(&e.to_string()),
    };

    // Build into scratch first, independent of the worktree below — a
    // worktree's `.git` file would otherwise confuse prepare_out_dir's
    // "does this look like a prior build" check.
    let build_dir = root.join(".soothfast").join("gh-deploy-build");
    let input = match assemble(&root, baseline, Some(&build_dir), nav::Missing::Fail) {
        Ok(i) => i,
        Err(e) => return err(&e),
    };
    let report = match build(&input) {
        Ok(r) => r,
        Err(e) => return err(&e),
    };
    report_warnings(&report);

    let wt = root.join(".soothfast").join("gh-deploy-worktree");
    let wt_str = match wt.to_str() {
        Some(s) => s,
        None => return err("worktree path is not UTF-8"),
    };
    if wt.exists() {
        let _ = invoke::git(&["worktree", "remove", "--force", wt_str]);
    }

    let result = deploy_worktree(&root, &wt, branch, remote, message, &build_dir, &report);
    let _ = invoke::git(&["worktree", "remove", "--force", wt_str]);
    match result {
        Ok(Some((pages, assets))) => {
            println!(
                "gh-deploy: published {pages} page(s), {assets} asset(s) to {remote}/{branch}"
            );
            0
        }
        Ok(None) => {
            println!("gh-deploy: no changes to publish");
            0
        }
        Err(e) => err(&e),
    }
}

fn deploy_worktree(
    root: &Path,
    wt: &Path,
    branch: &str,
    remote: &str,
    message: Option<&str>,
    build_dir: &Path,
    report: &soothfast_site::BuildReport,
) -> Result<Option<(usize, usize)>, String> {
    let wt_str = wt.to_str().ok_or("worktree path is not UTF-8")?;

    // Best-effort: a fetch failure (offline, first-ever deploy) just means
    // we fall through to the orphan-branch path below.
    let _ = invoke::git(&["fetch", remote, branch]);
    let remote_ref = format!("refs/remotes/{remote}/{branch}");
    let has_remote_branch = invoke::git(&["rev-parse", "--verify", &remote_ref]).is_ok();

    if has_remote_branch {
        invoke::git(&["worktree", "add", "-B", branch, wt_str, &remote_ref])
            .map_err(|e| e.to_string())?;
    } else {
        invoke::git(&["worktree", "add", "--detach", wt_str, "HEAD"]).map_err(|e| e.to_string())?;
        invoke::git_in(wt, &["checkout", "--orphan", branch]).map_err(|e| e.to_string())?;
        invoke::git_in(wt, &["rm", "-rf", "-q", "."]).map_err(|e| e.to_string())?;
    }

    clear_worktree(wt)?;
    copy_dir_all(build_dir, wt)?;

    invoke::git_in(wt, &["add", "-A"]).map_err(|e| e.to_string())?;
    let status = invoke::git_in(wt, &["status", "--porcelain"]).map_err(|e| e.to_string())?;
    if status.is_empty() {
        return Ok(None);
    }

    let msg = message
        .map(str::to_string)
        .unwrap_or_else(|| default_message(root));
    invoke::git_in(wt, &["commit", "-q", "-m", &msg]).map_err(|e| e.to_string())?;
    invoke::git_in(wt, &["push", remote, &format!("HEAD:refs/heads/{branch}")])
        .map_err(|e| e.to_string())?;
    Ok(Some((report.pages, report.assets)))
}

/// Delete everything in `wt` except `.git`, so a stale prior deploy's pages
/// can't survive alongside the fresh build.
fn clear_worktree(wt: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(wt).map_err(|e| format!("{}: {e}", wt.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        result.map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            std::fs::create_dir_all(&to).map_err(|e| format!("{}: {e}", to.display()))?;
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| format!("{}: {e}", to.display()))?;
        }
    }
    Ok(())
}

fn default_message(root: &Path) -> String {
    match invoke::git_in(root, &["rev-parse", "--short", "HEAD"]) {
        Ok(sha) if !sha.is_empty() => format!("docs: publish site for {sha}"),
        _ => "docs: publish site".to_string(),
    }
}

fn err(msg: &str) -> i32 {
    eprintln!("soothfast: {msg}");
    1
}
