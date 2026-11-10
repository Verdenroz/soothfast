//! Trend time series: one JSONL record per `trend append` (typically per
//! merge to main), rendered as first→last drift per item. Chart rendering
//! lives in `soothfast-report`; the series format is the contract.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

use crate::invoke::{self, CommonArgs};

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("append") => append(&args[1..]),
        Some("render") => render(),
        _ => {
            eprintln!(
                "soothfast: usage: cargo soothfast trend append|render [-p PKG] [--from-baseline NAME]"
            );
            2
        }
    }
}

/// Branch the built site is published to; the series lives at its root.
pub const PAGES_BRANCH: &str = "gh-pages";
/// Remote `PAGES_BRANCH` is fetched from.
pub const PAGES_REMOTE: &str = "origin";

const SERIES_FILE: &str = "trend.jsonl";

/// The local working copy of the series.
pub fn working_copy(root: &Path) -> PathBuf {
    root.join(".soothfast").join(SERIES_FILE)
}

/// The series text: the working copy when it exists, otherwise the file at
/// the tip of the Pages branch. `None` means no series exists yet. An error
/// means one may exist but could not be read, which callers must not treat
/// as empty.
pub fn load(root: &Path) -> Result<Option<String>, String> {
    load_from(root, PAGES_REMOTE, PAGES_BRANCH)
}

/// The parsed series points, in recorded order.
pub fn load_points(root: &Path) -> Result<Vec<Value>, String> {
    Ok(parse_points(&load(root)?.unwrap_or_default()))
}

pub fn parse_points(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn load_from(root: &Path, remote: &str, branch: &str) -> Result<Option<String>, String> {
    let local = working_copy(root);
    if local.exists() {
        return std::fs::read_to_string(&local)
            .map(Some)
            .map_err(|e| format!("{}: {e}", local.display()));
    }
    read_branch(root, remote, branch)
}

fn git_output(root: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new("git")
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))
}

fn git_failure(args: &[&str], out: &Output) -> String {
    format!(
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

fn read_branch(root: &Path, remote: &str, branch: &str) -> Result<Option<String>, String> {
    let tip = format!("refs/remotes/{remote}/{branch}");
    let verify = ["rev-parse", "--verify", "--quiet", &tip];
    if !git_output(root, &verify)?.status.success() {
        let ls = ["ls-remote", "--exit-code", "--heads", remote, branch];
        let out = git_output(root, &ls)?;
        match out.status.code() {
            Some(0) => {}
            // `--exit-code` reports 2 when the remote answered and has no such branch.
            Some(2) => return Ok(None),
            _ => return Err(git_failure(&ls, &out)),
        }
        let spec = format!("+refs/heads/{branch}:{tip}");
        let fetch = ["fetch", "--depth", "1", remote, &spec];
        let out = git_output(root, &fetch)?;
        if !out.status.success() {
            return Err(git_failure(&fetch, &out));
        }
    }
    let path = format!("{tip}:{SERIES_FILE}");
    let ls_tree = ["ls-tree", "--name-only", &tip, SERIES_FILE];
    let out = git_output(root, &ls_tree)?;
    if !out.status.success() {
        return Err(git_failure(&ls_tree, &out));
    }
    if out.stdout.is_empty() {
        return Ok(None);
    }
    let show = ["show", &path];
    let out = git_output(root, &show)?;
    if !out.status.success() {
        return Err(git_failure(&show, &out));
    }
    Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
}

fn append(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut from_baseline: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if common.try_parse(a, &mut it) {
            continue;
        }
        match a.as_str() {
            "--from-baseline" => match it.next() {
                Some(n) => from_baseline = Some(n.clone()),
                None => {
                    eprintln!("soothfast: --from-baseline needs a name");
                    return 2;
                }
            },
            other => {
                eprintln!("soothfast: unknown trend arg {other:?}");
                return 2;
            }
        }
    }

    let commit =
        invoke::git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|_| "unknown".into());
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // A saved baseline already carries every backend's metrics; reading it
    // spares the pipeline a full re-measurement of the same commit.
    let (record, count) = if let Some(name) = &from_baseline {
        let doc = match invoke::load_baseline(name) {
            Ok(Some(d)) => d,
            Ok(None) => {
                eprintln!("soothfast: no baseline named {name:?}");
                return 2;
            }
            Err(e) => {
                eprintln!("soothfast: failed to load baseline: {e}");
                return 1;
            }
        };
        match point_from_baseline(&doc, common.pkg.as_deref(), &commit, unix) {
            Ok(r) => {
                let count = r["items"].as_object().map(|o| o.len()).unwrap_or(0);
                (r, count)
            }
            Err(e) => {
                eprintln!("soothfast: baseline {name:?}: {e}");
                return 2;
            }
        }
    } else {
        let records = match invoke::run_bench(&common, &[]) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("soothfast: {e}");
                return 1;
            }
        };
        let run = invoke::collect(&records);
        let record = json!({
            "unix": unix,
            "commit": commit,
            "noise_pct": run.noise_pct,
            "items": invoke::run_to_items_value(&run),
        });
        (record, run.items.len())
    };

    let root = match invoke::workspace_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 1;
        }
    };
    let mut line = record.to_string();
    line.push('\n');
    let path = match append_line(&root, PAGES_REMOTE, PAGES_BRANCH, &line) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 1;
        }
    };
    println!(
        "trend: appended {count} item(s) at commit {commit} -> {}",
        path.display()
    );
    0
}

/// Appends `line` to the working copy, first seeding it from the Pages
/// branch when no copy exists. A branch that cannot be read is an error:
/// appending to an empty series would let the next publish erase history.
fn append_line(root: &Path, remote: &str, branch: &str, line: &str) -> Result<PathBuf, String> {
    let existing = load_from(root, remote, branch)
        .map_err(|e| format!("cannot read the existing trend series: {e}"))?
        .unwrap_or_default();
    let path = working_copy(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&path, existing + line).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// A trend point built from a saved baseline instead of a fresh measurement.
/// Buildcost pseudo-items and other packages' entries are dropped so the
/// point holds exactly what `trend append -p PKG` would have measured;
/// imported items belong to no package and always stay.
fn point_from_baseline(
    doc: &Value,
    pkg: Option<&str>,
    commit: &str,
    unix: u64,
) -> Result<Value, String> {
    let items: serde_json::Map<String, Value> = doc["items"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|(id, _)| !id.starts_with("buildcost::"))
        .filter(|(id, item)| {
            item["imported"].is_string()
                || pkg.is_none_or(|p| invoke::id_pkg(id) == p.replace('-', "_").as_str())
        })
        .collect();
    if items
        .values()
        .all(|item| pkg.is_some() && item["imported"].is_string())
    {
        return Err(format!(
            "no bench items{} — measure into it first",
            pkg.map(|p| format!(" for package {p:?}"))
                .unwrap_or_default()
        ));
    }
    Ok(json!({
        "unix": unix,
        "commit": commit,
        "noise_pct": doc["noise_pct"],
        "items": items,
    }))
}

fn render() -> i32 {
    let points = match invoke::workspace_root()
        .map_err(|e| e.to_string())
        .and_then(|root| load_points(&root))
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 1;
        }
    };
    if points.is_empty() {
        eprintln!("soothfast: trend series is empty");
        return 2;
    }

    println!(
        "trend: {} point(s), {} -> {}",
        points.len(),
        points[0]["commit"].as_str().unwrap_or("?"),
        points[points.len() - 1]["commit"].as_str().unwrap_or("?"),
    );
    // Per item+metric: first -> last with relative drift.
    let mut ids: Vec<String> = points
        .iter()
        .flat_map(|p| {
            p["items"]
                .as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>())
        })
        .flatten()
        .collect();
    ids.sort();
    ids.dedup();

    for id in &ids {
        for (metric, path_keys) in [
            ("instructions", ["perfcnt", "instructions"]),
            ("walltime_median_ns", ["walltime", "median_ns"]),
            ("allocs", ["alloc", "allocs"]),
            ("size_bytes", ["buildcost", "size_bytes"]),
        ] {
            let series: Vec<f64> = points
                .iter()
                .filter_map(|p| p["items"][id][path_keys[0]][path_keys[1]].as_f64())
                .collect();
            if series.len() < 2 {
                continue;
            }
            let (first, last) = (series[0], series[series.len() - 1]);
            if first <= 0.0 {
                continue;
            }
            let drift = (last - first) / first * 100.0;
            println!(
                "{id:<44} {metric:<20} {first:.1} -> {last:.1} ({drift:+.1}% over {} pts)",
                series.len()
            );
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use serde_json::json;

    use super::{append_line, load_from, point_from_baseline, working_copy};

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("soothfast-trend-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A work repo whose `origin` is a bare repo; `series` is committed as
    /// `trend.jsonl` on its `gh-pages` branch when given.
    fn repo_with_origin(name: &str, series: Option<&str>) -> PathBuf {
        let base = scratch(name);
        let origin = base.join("origin.git");
        let work = base.join("work");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&origin, &["init", "-q", "--bare"]);
        git(&work, &["init", "-q"]);
        git(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&work, &["commit", "-q", "--allow-empty", "-m", "init"]);
        if let Some(text) = series {
            git(&work, &["checkout", "-q", "--orphan", "gh-pages"]);
            std::fs::write(work.join("trend.jsonl"), text).unwrap();
            git(&work, &["add", "trend.jsonl"]);
            git(&work, &["commit", "-q", "-m", "series"]);
            git(&work, &["push", "-q", "origin", "gh-pages"]);
            git(&work, &["update-ref", "-d", "refs/remotes/origin/gh-pages"]);
        }
        work
    }

    #[test]
    fn the_local_copy_is_read_without_touching_the_remote() {
        let root = scratch("local");
        std::fs::create_dir_all(root.join(".soothfast")).unwrap();
        std::fs::write(working_copy(&root), "{\"a\":1}\n").unwrap();
        let text = load_from(&root, "origin", "gh-pages").unwrap();
        assert_eq!(text.as_deref(), Some("{\"a\":1}\n"));
    }

    #[test]
    fn the_branch_tip_is_fetched_when_there_is_no_local_copy() {
        let root = repo_with_origin("tip", Some("{\"b\":2}\n"));
        let text = load_from(&root, "origin", "gh-pages").unwrap();
        assert_eq!(text.as_deref(), Some("{\"b\":2}\n"));
    }

    #[test]
    fn a_missing_branch_is_an_empty_series() {
        let root = repo_with_origin("missing", None);
        assert_eq!(load_from(&root, "origin", "gh-pages").unwrap(), None);
    }

    #[test]
    fn an_unreachable_remote_is_an_error() {
        let root = repo_with_origin("unreadable", None);
        git(
            &root,
            &["remote", "set-url", "origin", "/nonexistent/origin.git"],
        );
        assert!(load_from(&root, "origin", "gh-pages").is_err());
    }

    #[test]
    fn append_seeds_the_working_copy_from_the_branch() {
        let root = repo_with_origin("seed", Some("{\"b\":2}\n"));
        let path = append_line(&root, "origin", "gh-pages", "{\"c\":3}\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "{\"b\":2}\n{\"c\":3}\n"
        );
    }

    #[test]
    fn append_fails_without_writing_when_the_branch_cannot_be_read() {
        let root = repo_with_origin("noappend", None);
        git(
            &root,
            &["remote", "set-url", "origin", "/nonexistent/origin.git"],
        );
        assert!(append_line(&root, "origin", "gh-pages", "{}\n").is_err());
        assert!(!working_copy(&root).exists());
    }

    fn baseline() -> serde_json::Value {
        json!({
            "version": 1,
            "noise_pct": 0.42,
            "items": {
                "finance_query::indicators::sma": {
                    "fingerprint": "abc",
                    "perfcnt": { "instructions": 1000, "cycles": 900, "cache_refs": 10 },
                    "walltime": { "median_ns": 50.0, "mad_ns": 1.0, "p99_ns": 70.0 },
                    "alloc": { "allocs": 2, "bytes": 128 }
                },
                "other_pkg::thing": { "walltime": { "median_ns": 9.0 } },
                "jmh::A.b": { "imported": "jmh", "walltime": { "median_ns": 5.0 } },
                "buildcost::finance-query::default": {
                    "buildcost": { "build_ms": 12000, "size_bytes": 4096 }
                }
            }
        })
    }

    #[test]
    fn a_point_carries_the_baselines_metrics_verbatim() {
        let p = point_from_baseline(&baseline(), Some("finance-query"), "abc1234", 1700).unwrap();
        assert_eq!(p["unix"], 1700);
        assert_eq!(p["commit"], "abc1234");
        assert_eq!(p["noise_pct"], 0.42);
        let item = &p["items"]["finance_query::indicators::sma"];
        assert_eq!(item["perfcnt"]["instructions"], 1000);
        assert_eq!(item["walltime"]["median_ns"], 50.0);
        assert_eq!(item["alloc"]["allocs"], 2);
    }

    #[test]
    fn out_of_scope_entries_are_dropped() {
        // `-p` scopes the point the way it scopes a measured run: no other
        // packages, no buildcost pseudo-items.
        let p = point_from_baseline(&baseline(), Some("finance-query"), "c", 0).unwrap();
        let items = p["items"].as_object().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.contains_key("finance_query::indicators::sma"));
        assert!(!items.contains_key("other_pkg::thing"));
    }

    #[test]
    fn imported_items_stay_under_a_package_scope() {
        let p = point_from_baseline(&baseline(), Some("finance-query"), "c", 0).unwrap();
        assert_eq!(p["items"]["jmh::A.b"]["imported"], "jmh");
    }

    #[test]
    fn no_package_keeps_every_bench_item_but_never_buildcost() {
        let p = point_from_baseline(&baseline(), None, "c", 0).unwrap();
        let items = p["items"].as_object().unwrap();
        assert_eq!(items.len(), 3);
        assert!(!items.contains_key("buildcost::finance-query::default"));
    }

    #[test]
    fn an_empty_slice_is_an_error_not_an_empty_point() {
        let err = point_from_baseline(&baseline(), Some("absent-pkg"), "c", 0).unwrap_err();
        assert!(err.contains("absent-pkg"), "unhelpful error: {err}");
    }
}
