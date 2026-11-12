//! `cargo bench` and `cargo build` invocation, bench record collection, and
//! the sharded prebuild of bench executables.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

use super::{CommonArgs, Shard, list_items, timed};
use crate::benchexe;

/// Toolchain override for `cargo bench`, `SOOTHFAST_BENCH_TOOLCHAIN`. Unset
/// by default, so `measure`/`gate` stay pinned to the active toolchain.
pub(crate) fn bench_toolchain() -> Option<String> {
    std::env::var("SOOTHFAST_BENCH_TOOLCHAIN").ok()
}

/// `Measure` pins the bench profile so numbers stay comparable; `Discover`
/// builds under the dev profile, since a registry dump's output doesn't
/// depend on optimization and dev artifacts never displace measured ones.
#[derive(Clone, Copy)]
pub(crate) enum Build {
    Measure,
    Discover,
}

/// `cargo bench` carrying every input that selects which binary gets built.
/// Both the measured run and the `--no-run` digest probe go through here, so
/// they cannot drift into building different binaries.
pub(crate) fn bench_build_command(
    common: &CommonArgs,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
    build: Build,
    pkgs: &[&str],
) -> io::Result<Command> {
    cargo_command("bench", common, dir, target_dir, build, pkgs)
}

/// `cargo <verb>` for the bench target, carrying the same inputs for any verb.
fn cargo_command(
    verb: &str,
    common: &CommonArgs,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
    build: Build,
    pkgs: &[&str],
) -> io::Result<Command> {
    let mut cmd = Command::new("cargo");
    if let Some(toolchain) = bench_toolchain() {
        cmd.arg(format!("+{toolchain}"));
    }
    if let Build::Measure = build
        && let Some(n) = common.codegen_units_env()
    {
        for member in workspace_members(dir)? {
            cmd.arg("--config");
            cmd.arg(format!(
                "profile.bench.package.\"{member}\".codegen-units={n}"
            ));
        }
    }
    cmd.arg(verb);
    if let Some(td) = target_dir {
        cmd.env("CARGO_TARGET_DIR", td);
    }
    if let Build::Discover = build {
        cmd.args(["--profile", "dev"]);
    }
    for p in pkgs {
        cmd.args(["-p", p]);
    }
    // Cargo-level flag: must precede the `--` separating runner args.
    if let Some(f) = &common.features {
        cmd.args(["--features", f]);
    }
    let target = common.target.as_deref().unwrap_or("soothfast");
    cmd.args(["--bench", target]);
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    Ok(cmd)
}

/// Link the bench target a second time with the linker writing its map to
/// `map`, and name the executable. `cargo rustc` hands the flag to the bench
/// crate alone, so dependencies are not rebuilt, and the result is a separate
/// artifact next to the measured one.
pub fn build_link_map(
    common: &CommonArgs,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
    map: &Path,
) -> Option<PathBuf> {
    let target = common.target.as_deref().unwrap_or("soothfast");
    let pkgs: Vec<&str> = common.pkg.as_deref().into_iter().collect();
    let mut cmd = cargo_command("rustc", common, dir, target_dir, Build::Measure, &pkgs).ok()?;
    cmd.args(["--profile", "bench", "--message-format=json", "--"]);
    cmd.arg("-C");
    cmd.arg(format!("link-arg=-Wl,-Map={}", map.display()));
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    bench_artifact(&out.stdout, target)
}

/// The executable cargo's `--message-format=json` output names for the bench
/// target `target`.
pub(crate) fn bench_artifact(stdout: &[u8], target: &str) -> Option<PathBuf> {
    cargo_messages(stdout)
        .filter(|m| m["reason"] == "compiler-artifact")
        .filter(|m| m["target"]["name"] == target)
        .filter(|m| {
            m["target"]["kind"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k == "bench")
        })
        .find_map(|m| m["executable"].as_str().map(PathBuf::from))
}

/// Each line of cargo's `--message-format=json` output that parses as JSON.
pub fn cargo_messages(stdout: &[u8]) -> impl Iterator<Item = Value> + '_ {
    stdout
        .split(|&b| b == b'\n')
        .filter_map(|l| serde_json::from_str(&String::from_utf8_lossy(l)).ok())
}

/// Workspace member names, resolved once per tree. A gate that cannot name
/// them cannot pin them, and a build stamp claiming a pin that did not
/// happen would compare as if it had.
fn workspace_members(dir: Option<&Path>) -> io::Result<Vec<String>> {
    static HEAD: OnceLock<io::Result<Vec<String>>> = OnceLock::new();
    if dir.is_some() {
        return crate::workspace::members(dir);
    }
    match HEAD.get_or_init(|| crate::workspace::members(None)) {
        Ok(names) => Ok(names.clone()),
        Err(e) => Err(io::Error::other(e.to_string())),
    }
}

fn bench_command(
    common: &CommonArgs,
    extra: &[&str],
    dir: Option<&Path>,
    target_dir: Option<&Path>,
    build: Build,
) -> io::Result<Command> {
    let fill = matches!(build, Build::Discover);
    let mut cmd = match benchexe::run_command(common, dir, target_dir, build, fill) {
        Some(cmd) => cmd,
        None => {
            let pkgs: Vec<&str> = common.pkg.as_deref().into_iter().collect();
            let mut cmd = bench_build_command(common, dir, target_dir, build, &pkgs)?;
            cmd.arg("--");
            cmd
        }
    };
    if let Some(f) = &common.filter {
        cmd.args(["--filter", f]);
    }
    if let Some(b) = &common.backend {
        cmd.args(["--backend", b]);
    }
    if let Some(s) = &common.samples {
        cmd.args(["--samples", s]);
    }
    cmd.args(extra);
    Ok(cmd)
}

/// Run the bench binary with `--json` (optionally in another worktree) and
/// parse JSONL records.
pub fn run_bench_in(
    common: &CommonArgs,
    extra: &[&str],
    dir: Option<&Path>,
) -> io::Result<Vec<Value>> {
    run_bench_dir(common, extra, dir, None)
}

/// `run_bench_in` with an explicit CARGO_TARGET_DIR for the child build —
/// the gate points merge-base worktree builds at `worktree_target_dir()`.
pub fn run_bench_dir(
    common: &CommonArgs,
    extra: &[&str],
    dir: Option<&Path>,
    target_dir: Option<&Path>,
) -> io::Result<Vec<Value>> {
    run_records(common, extra, dir, target_dir, Build::Measure)
}

/// `run_bench_in` for registry dumps (`--list`, `--list-routes`): dev
/// profile, no codegen-units pin.
pub fn run_discovery_in(
    common: &CommonArgs,
    extra: &[&str],
    dir: Option<&Path>,
) -> io::Result<Vec<Value>> {
    run_records(common, extra, dir, None, Build::Discover)
}

/// `run_discovery_in` in the current workspace.
pub fn run_discovery(common: &CommonArgs, extra: &[&str]) -> io::Result<Vec<Value>> {
    run_discovery_in(common, extra, None)
}

/// Lines of a failed merge-base run's stderr shown from its first error:
/// enough for rustc's notes under it, which name what it rejected.
const BASE_ERROR_LINES: usize = 40;

/// Lines shown from the end of that stderr when they fall past the window.
const BASE_LAST_LINES: usize = 10;

/// Report a failed merge-base bench: its build, which the seed may have
/// caused, or its run.
fn report_base_failure(stderr: &str, built: bool, target_dir: &Path) {
    if built {
        eprintln!("gate: the merge-base build failed; cargo reported:");
    } else {
        eprintln!("gate: the merge-base bench run failed; it reported:");
    }
    for line in failure_excerpt(stderr) {
        eprintln!("  {line}");
    }
    if built {
        crate::seed::report(target_dir);
    }
}

/// The one line that says why a bench failed: the excerpt's opening line.
fn failure_reason(stderr: &str) -> &str {
    stderr
        .lines()
        .find(|l| opens_failure(l))
        .or_else(|| stderr.trim().lines().last())
        .unwrap_or("no error output")
}

/// Whether `line` opens what a failure excerpt shows: an error, a panic, or
/// the runner's own message.
fn opens_failure(line: &str) -> bool {
    let line = crate::verdict::strip_ansi(line);
    let line = line.trim_start();
    line.starts_with("error:")
        || line.starts_with("error[")
        || (line.starts_with("soothfast runner:")
            && !line.starts_with("soothfast runner: warning:"))
        || (line.starts_with("thread '") && line.contains("' panicked"))
}

/// From the first line [`opens_failure`] matches, then the last lines, with
/// `...` between when they are not adjacent. Without such a line, the last
/// lines alone.
fn failure_excerpt(stderr: &str) -> Vec<&str> {
    let lines: Vec<&str> = stderr.lines().collect();
    let start = lines
        .iter()
        .position(|l| opens_failure(l))
        .unwrap_or(lines.len().saturating_sub(BASE_ERROR_LINES));
    let end = (start + BASE_ERROR_LINES).min(lines.len());
    let last = lines.len().saturating_sub(BASE_LAST_LINES).max(end);
    let mut shown = lines[start..end].to_vec();
    if last > end {
        shown.push("...");
    }
    shown.extend(&lines[last..]);
    shown
}

fn run_records(
    common: &CommonArgs,
    extra: &[&str],
    dir: Option<&Path>,
    target_dir: Option<&Path>,
    build: Build,
) -> io::Result<Vec<Value>> {
    let mut args = vec!["--json"];
    args.extend_from_slice(extra);
    let owned = match common.shard {
        Some(shard) if matches!(build, Build::Measure) && !extra.contains(&"--id") => {
            let Some(owned) = shard_ids(common, shard, dir, target_dir) else {
                return Err(io::Error::other(format!(
                    "--shard {shard}: the bench binary lists no item (check --filter and the registry)"
                )));
            };
            if owned.is_empty() {
                return Ok(Vec::new());
            }
            owned
        }
        _ => Vec::new(),
    };
    args.extend(owned.iter().flat_map(|id| ["--id", id.as_str()]));
    let target = common.target.as_deref().unwrap_or("soothfast");
    let base = dir.and(target_dir);
    // On a store miss the merge-base builds in a step of its own, so only a
    // failed build, which the seed may have caused, gets the seed report.
    if let Some(td) = base
        && !benchexe::is_stored(common, dir, target_dir, build)
    {
        match timed("cargo bench build", || {
            benchexe::build_only(common, dir, target_dir, build)
        }) {
            Ok(()) => {}
            Err(benchexe::NoBuild::Failed(stderr)) => {
                report_base_failure(&stderr, true, td);
                return Err(io::Error::other(format!(
                    "the merge-base build failed: {} (is the [[bench]] name = \"{target}\", harness = false target set up?)",
                    failure_reason(&stderr)
                )));
            }
            Err(benchexe::NoBuild::Other(why)) => {
                return Err(io::Error::other(format!(
                    "the merge-base bench was not built: {why}"
                )));
            }
        }
    }
    let mut cmd = bench_command(common, &args, dir, target_dir, build)?;
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let label = if base.is_some() {
        "cargo bench run"
    } else {
        "cargo bench (build and run)"
    };
    let out = timed(label, || cmd.output())?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if let Some(td) = base {
            report_base_failure(&stderr, false, td);
            return Err(io::Error::other(format!(
                "the merge-base bench run failed with {}: {}",
                out.status,
                failure_reason(&stderr)
            )));
        }
        let reason = failure_reason(&stderr);
        return Err(io::Error::other(format!(
            "cargo bench failed with {}: {reason} (is the [[bench]] name = \"{target}\", harness = false target set up?)",
            out.status
        )));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // A '{'-prefixed line that fails to parse means truncated/corrupt runner
    // output; dropping it could silently erase a regression record, so error.
    let mut records = Vec::new();
    for l in stdout.lines().filter(|l| l.starts_with('{')) {
        match serde_json::from_str(l) {
            Ok(v) => records.push(v),
            Err(e) => {
                let snippet: String = l.chars().take(200).collect();
                return Err(io::Error::other(format!(
                    "malformed runner record ({e}): {snippet}"
                )));
            }
        }
    }
    Ok(records)
}

/// Run the bench binary in raw (non-JSON) mode and capture stdout as text.
pub fn run_bench_raw(common: &CommonArgs, extra: &[&str]) -> io::Result<String> {
    let mut cmd = bench_command(common, extra, None, None, Build::Measure)?;
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let out = cmd.output()?;
    if !out.status.success() {
        // The runner's die() message lands on stderr; surface it (e.g. the
        // real callgrind SIGILL reason) instead of a bare exit status.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let reason = stderr
            .lines()
            .find(|l| l.starts_with("soothfast runner:"))
            .unwrap_or("")
            .to_string();
        return Err(io::Error::other(format!(
            "cargo bench failed with {}{}{}",
            out.status,
            if reason.is_empty() { "" } else { ": " },
            reason
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn run_bench(common: &CommonArgs, extra: &[&str]) -> io::Result<Vec<Value>> {
    run_bench_in(common, extra, None)
}

/// Build several packages' bench targets in one cargo invocation. Built one
/// at a time they serialize, each paying its own critical path.
pub fn prebuild_benches(
    pkgs: &[String],
    common: &CommonArgs,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
) -> io::Result<()> {
    let names: Vec<&str> = pkgs
        .iter()
        .map(String::as_str)
        .filter(|pkg| {
            let one = CommonArgs {
                pkg: Some(pkg.to_string()),
                ..common.clone()
            };
            !benchexe::is_stored(&one, dir, target_dir, Build::Measure)
        })
        .collect();
    if names.is_empty() {
        return Ok(());
    }
    let status = bench_build_command(common, dir, target_dir, Build::Measure, &names)?
        .args(["--no-run"])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "cargo bench --no-run exited {status}"
        )))
    }
}

/// The ids of the bench binary's items that `shard` owns, listed once per
/// binary. `None` when it lists no item at all, a setup error for every shard.
fn shard_ids(
    common: &CommonArgs,
    shard: Shard,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
) -> Option<Vec<String>> {
    let exe = bench_executable(common, dir, target_dir)?;
    let listed = listed_ids(&exe, common.filter.as_deref())?;
    if listed.is_empty() {
        return None;
    }
    Some(listed.into_iter().filter(|id| shard.owns(id)).collect())
}

/// Every item id `exe` lists under `filter`, whichever shard owns it, listed
/// once per binary.
pub fn listed_ids(exe: &Path, filter: Option<&str>) -> Option<Vec<String>> {
    type Key = (PathBuf, Option<String>);
    static LISTED: OnceLock<Mutex<HashMap<Key, Vec<String>>>> = OnceLock::new();
    let mut memo = LISTED.get_or_init(Default::default).lock().ok()?;
    let key = (exe.to_path_buf(), filter.map(str::to_string));
    if let Some(ids) = memo.get(&key) {
        return Some(ids.clone());
    }
    let ids: Vec<String> = list_items(exe, filter)?.into_keys().collect();
    memo.insert(key, ids.clone());
    Some(ids)
}

/// Path of the compiled bench executable, from the binary store or else via
/// `cargo bench --no-run`'s JSON messages (see `benchexe`). `None` on any
/// failure: callers fall through to a normal measured run, which surfaces the
/// real error.
pub fn bench_executable(
    common: &CommonArgs,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
) -> Option<PathBuf> {
    benchexe::executable(common, dir, target_dir, Build::Measure)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_failed_build_is_shown_from_its_first_error_and_its_last_lines() {
        let mut lines: Vec<String> = (0..5).map(|i| format!("   Compiling c{i}")).collect();
        lines.push("\u{1b}[1m\u{1b}[91merror[E0463]\u{1b}[0m: can't find crate".into());
        lines.extend((0..60).map(|i| format!("note {i}")));
        let stderr = lines.join("\n");
        let shown = super::failure_excerpt(&stderr);
        assert_eq!(
            shown.len(),
            super::BASE_ERROR_LINES + 1 + super::BASE_LAST_LINES
        );
        assert!(shown[0].contains("E0463"), "{shown:?}");
        assert_eq!(shown[super::BASE_ERROR_LINES], "...");
        assert_eq!(shown.last(), Some(&"note 59"));
        assert_eq!(super::failure_excerpt("a\nb"), ["a", "b"]);
        let panic = "warming up\nthread 'main' panicked at src/x.rs:1:1:\nboom";
        assert_eq!(
            super::failure_excerpt(panic)[0],
            "thread 'main' panicked at src/x.rs:1:1:"
        );
        let runner = "\x1b]8;;file://x\x1b\\link\x1b]8;;\x1b\\\n\
                      errors: 0\n   Compiling thiserror v2.0.20\n\
                      soothfast runner: warning: slow\n  soothfast runner: no such item";
        assert_eq!(
            super::failure_excerpt(runner),
            ["  soothfast runner: no such item"]
        );
    }
}
