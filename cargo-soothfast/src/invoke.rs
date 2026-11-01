//! Subprocess plumbing: run the user's soothfast bench binary via `cargo bench`,
//! parse its JSONL, and read/write baseline files.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Instant;

use serde_json::{Value, json};

use crate::{benchexe, rustdoc_inputs, rustdocstore, store};

/// Run `f`, printing its wall time to stderr when `SOOTHFAST_TIMINGS` is set.
pub fn timed<T>(label: &str, f: impl FnOnce() -> T) -> T {
    if std::env::var_os("SOOTHFAST_TIMINGS").is_none() {
        return f();
    }
    let start = Instant::now();
    let out = f();
    eprintln!(
        "soothfast-timing: {label} {:.1}s",
        start.elapsed().as_secs_f64()
    );
    out
}

/// Flags shared by `measure` and `gate`.
#[derive(Default, Clone)]
pub struct CommonArgs {
    pub pkg: Option<String>,
    pub filter: Option<String>,
    pub backend: Option<String>,
    pub samples: Option<String>,
    pub features: Option<String>,
    /// `[[bench]]` target to link the registry from (default `"soothfast"`).
    pub target: Option<String>,
    /// `codegen-units` measurement builds are pinned to, or `"inherit"` to
    /// leave the project's own profile alone.
    pub codegen_units: Option<String>,
    /// `--no-item-cache`: measure every item even where a stored measurement
    /// of the same executed code exists.
    pub no_item_cache: bool,
    /// `--shard I/N`: measure and gate only the items this shard owns.
    pub shard: Option<Shard>,
}

/// One of `count` disjoint item sets, numbered from 1. An item belongs to the
/// shard its id hashes to, so an item that left the registry still has one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shard {
    pub index: u64,
    pub count: u64,
}

impl Shard {
    pub fn parse(s: &str) -> Result<Shard, String> {
        let bad = || format!("--shard wants I/N with 1 <= I <= N, got {s:?}");
        let (i, n) = s.split_once('/').ok_or_else(bad)?;
        let index: u64 = i.parse().map_err(|_| bad())?;
        let count: u64 = n.parse().map_err(|_| bad())?;
        if index == 0 || index > count {
            return Err(bad());
        }
        Ok(Shard { index, count })
    }

    pub fn owns(&self, id: &str) -> bool {
        soothfast_registry::fnv1a(id.as_bytes()) % self.count == self.index - 1
    }
}

impl std::fmt::Display for Shard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.index, self.count)
    }
}

impl CommonArgs {
    /// Consume one flag if it belongs to the common set.
    pub fn try_parse<'a>(&mut self, arg: &str, it: &mut impl Iterator<Item = &'a String>) -> bool {
        let slot = match arg {
            "--no-item-cache" => {
                self.no_item_cache = true;
                return true;
            }
            "-p" | "--package" => &mut self.pkg,
            "--filter" => &mut self.filter,
            "--backend" => &mut self.backend,
            "--samples" => &mut self.samples,
            "--features" => &mut self.features,
            // `--bench` is the unambiguous spelling and works everywhere.
            // `--target` means the same thing here for compatibility, but
            // in `sdk build` it means a Rust triple the way cargo spells
            // it — which is why the other name had to exist.
            "--bench" | "--target" => &mut self.target,
            "--codegen-units" => &mut self.codegen_units,
            _ => return false,
        };
        *slot = it.next().cloned();
        true
    }

    /// The `codegen-units` to force on a measurement build, or `None` to
    /// inherit the project's profile.
    pub fn codegen_units_env(&self) -> Option<&str> {
        match self.codegen_units.as_deref() {
            Some("inherit") => None,
            Some(n) => Some(n),
            None => Some(CANONICAL_CODEGEN_UNITS),
        }
    }

    /// The pin as a build stamp records it: value and scope together, so a
    /// baseline taken under a different scope cannot read as comparable.
    pub fn codegen_units_stamp(&self) -> Option<String> {
        self.codegen_units_env().map(|n| format!("{n}/members"))
    }
}

/// Codegen partitioning varies with unrelated edits: rustc reassigns modules
/// to units whenever a crate's contents change, which moves inlining and
/// register allocation in functions nobody touched. One unit cannot be
/// repartitioned, so both sides of a comparison are pinned to it.
const CANONICAL_CODEGEN_UNITS: &str = "1";

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
    Some(
        listing(exe, filter)?
            .into_iter()
            .map(|(id, _)| id)
            .collect(),
    )
}

/// A listed item's id, and the package that registered it when the runner
/// says.
pub type Listed = (String, Option<String>);

/// [`listed_ids`] with each item's registering package.
pub fn listing(exe: &Path, filter: Option<&str>) -> Option<Vec<Listed>> {
    type Key = (PathBuf, Option<String>);
    static LISTED: OnceLock<Mutex<HashMap<Key, Vec<Listed>>>> = OnceLock::new();
    let mut memo = LISTED.get_or_init(Default::default).lock().ok()?;
    let key = (exe.to_path_buf(), filter.map(str::to_string));
    if let Some(listed) = memo.get(&key) {
        return Some(listed.clone());
    }
    let listed: Vec<Listed> = item_records(exe, filter)?
        .iter()
        .filter(|v| v["fingerprint"].is_string())
        .filter_map(|v| {
            let id = v["id"].as_str()?.to_string();
            Some((id, v["pkg"].as_str().map(str::to_string)))
        })
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect();
    memo.insert(key, listed.clone());
    Some(listed)
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

/// What a bench binary resolves on this host, from the env line a run starts
/// with.
#[derive(Debug, Clone, PartialEq)]
pub struct HostEnv {
    pub gating_backend: String,
    /// Set only when callgrind gates and the harness can see its guest.
    pub guest: Option<Guest>,
    /// How callgrind's per-iteration Ir is taken, when callgrind gates. A
    /// harness that does not name it only has the two-process method.
    pub callgrind_method: Option<String>,
    /// Whether the harness takes `--id` and reports `executed` records.
    pub item_reuse: bool,
}

/// What callgrind's counts depend on besides the binary.
#[derive(Debug, Clone, PartialEq)]
pub struct Guest {
    pub cpu: String,
    pub glibc: String,
    /// Digest of the loaded libc file; distro rebuilds keep `glibc`.
    pub libc: String,
    pub valgrind: String,
}

impl HostEnv {
    /// The method callgrind's counts come from.
    pub fn method(&self) -> &str {
        self.callgrind_method.as_deref().unwrap_or("two-process")
    }

    fn from_line(v: &Value) -> Option<Self> {
        let field = |k: &str| v[k].as_str().map(String::from);
        let guest = match (
            field("guest_cpu"),
            field("guest_glibc"),
            field("guest_libc"),
            field("valgrind"),
        ) {
            (Some(cpu), Some(glibc), Some(libc), Some(valgrind)) => Some(Guest {
                cpu,
                glibc,
                libc,
                valgrind,
            }),
            _ => None,
        };
        Some(HostEnv {
            gating_backend: field("gating_backend")?,
            guest,
            callgrind_method: field("callgrind_method"),
            item_reuse: v["item_reuse"].as_bool().unwrap_or(false),
        })
    }
}

/// What `exe` resolves on this host for `backend`, as the env line of a real
/// run would name it. A harness older than `--env`, or a named backend this
/// host cannot run, exits non-zero and reads as unresolved.
pub fn host_env(exe: &Path, backend: Option<&str>) -> Result<HostEnv, String> {
    let mut cmd = Command::new(exe);
    cmd.arg("--env");
    if let Some(b) = backend {
        cmd.args(["--backend", b]);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("could not run the bench harness: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = stderr.lines().last().unwrap_or("").trim();
        return Err(format!("bench harness does not answer --env ({why})"));
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["type"] == "env")
        .and_then(|v| HostEnv::from_line(&v))
        .ok_or_else(|| "bench harness printed no env line for --env".into())
}

/// The items `exe` registers with their fingerprints, narrowed by `filter`
/// the way a run is.
pub fn list_items(exe: &Path, filter: Option<&str>) -> Option<BTreeMap<String, String>> {
    Some(
        item_records(exe, filter)?
            .iter()
            .filter_map(|v| {
                Some((
                    v["id"].as_str()?.to_string(),
                    v["fingerprint"].as_str()?.to_string(),
                ))
            })
            .collect(),
    )
}

/// The `item` records of `exe --list --json` under `filter`.
fn item_records(exe: &Path, filter: Option<&str>) -> Option<Vec<Value>> {
    let mut cmd = Command::new(exe);
    cmd.args(["--list", "--json"]);
    if let Some(f) = filter {
        cmd.args(["--filter", f]);
    }
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["type"] == "item")
            .collect(),
    )
}

/// One item's collected metrics across backends.
#[derive(Default, Debug, Clone)]
pub struct ItemMetrics {
    pub fingerprint: String,
    pub covers: String,
    pub median_ns: Option<f64>,
    pub mad_ns: Option<f64>,
    pub p99_ns: Option<f64>,
    pub instructions: Option<u64>,
    pub cycles: Option<u64>,
    pub cache_refs: Option<u64>,
    pub ir: Option<u64>,
    /// Iterations `ir` averages over, when the harness says.
    pub window: Option<u64>,
    pub allocs: Option<u64>,
    pub bytes: Option<u64>,
    pub polls: Option<u64>,
    pub wakes: Option<u64>,
    /// Gate threshold widened by `#[bench(tolerance = "...")]`.
    pub tolerance_pct: Option<f64>,
    /// Per-round walltime medians when a side ran interleaved rounds.
    pub wall_rounds: Vec<f64>,
    /// buildcost pseudo-items
    pub build_ms: Option<u64>,
    pub size_bytes: Option<u64>,
    /// Set when this item alone came from a stored measurement.
    pub reused: Option<Reused>,
    /// Id prefix of the `import` that produced this item.
    pub imported: Option<String>,
    /// Package whose code registered the item, when the runner says.
    pub pkg: Option<String>,
}

/// One assertion verdict from the runner.
#[derive(Debug, Clone)]
pub struct AssertionOutcome {
    pub id: String,
    pub kind: String,
    pub ok: bool,
    pub detail: String,
}

impl AssertionOutcome {
    /// The verdict with its item id, as a baseline or a stored run keeps it.
    pub fn to_json(&self) -> Value {
        json!({ "id": self.id, "kind": self.kind, "ok": self.ok, "detail": self.detail })
    }

    /// The verdict without its item id, as a per-item store keeps it.
    pub fn verdict_json(&self) -> Value {
        json!({ "kind": self.kind, "ok": self.ok, "detail": self.detail })
    }

    /// A verdict of item `id` read back from [`AssertionOutcome::verdict_json`].
    pub fn from_verdict_json(id: &str, v: &Value) -> Option<AssertionOutcome> {
        Some(AssertionOutcome {
            id: id.to_string(),
            kind: v["kind"].as_str()?.to_string(),
            ok: v["ok"].as_bool()?,
            detail: v["detail"].as_str()?.to_string(),
        })
    }
}

/// An `[object, symbol]` pair of an `executed` record.
pub fn pair(v: &Value) -> Option<(String, String)> {
    Some((v[0].as_str()?.to_string(), v[1].as_str()?.to_string()))
}

/// A full measurement run, keyed by item ID.
#[derive(Default, Debug, Clone)]
pub struct Run {
    pub noise_pct: Option<f64>,
    pub build: Option<crate::buildstamp::BuildStamp>,
    pub gating_backend: Option<String>,
    pub items: BTreeMap<String, ItemMetrics>,
    pub assertions: Vec<AssertionOutcome>,
    /// The `(object, symbol)` pairs each item executed, from a callgrind run.
    pub executed: BTreeMap<String, Vec<(String, String)>>,
    /// Set when some metrics came from a stored run instead of this process.
    pub reused: Option<Reused>,
}

/// Which metrics of a run were taken from a stored run, and what it measured.
#[derive(Debug, Clone, PartialEq)]
pub struct Reused {
    pub from: String,
    pub metrics: Vec<&'static str>,
}

/// Fold raw runner records into per-item metrics.
pub fn collect(records: &[Value]) -> Run {
    let mut run = Run::default();
    for rec in records {
        match rec["type"].as_str() {
            Some("calibration") => run.noise_pct = rec["noise_pct"].as_f64(),
            Some("env") => run.gating_backend = rec["gating_backend"].as_str().map(str::to_string),
            Some("assertion") => run.assertions.push(AssertionOutcome {
                id: rec["id"].as_str().unwrap_or("").to_string(),
                kind: rec["kind"].as_str().unwrap_or("").to_string(),
                ok: rec["ok"].as_bool().unwrap_or(false),
                detail: rec["detail"].as_str().unwrap_or("").to_string(),
            }),
            Some("executed") if run.gating_backend.as_deref() == Some("callgrind") => {
                if let Some(id) = rec["id"].as_str() {
                    let functions = rec["functions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(pair)
                        .collect();
                    run.executed.insert(id.to_string(), functions);
                }
            }
            Some("result") => {
                let Some(id) = rec["id"].as_str() else {
                    continue;
                };
                let item = run.items.entry(id.to_string()).or_default();
                if let Some(fp) = rec["fingerprint"].as_str() {
                    item.fingerprint = fp.to_string();
                }
                if let Some(c) = rec["covers"].as_str() {
                    item.covers = c.to_string();
                }
                if let Some(p) = rec["pkg"].as_str() {
                    item.pkg = Some(p.to_string());
                }
                if let Some(t) = rec["tolerance_pct"].as_f64() {
                    item.tolerance_pct = Some(t);
                }
                let m = &rec["metrics"];
                match rec["backend"].as_str() {
                    Some("walltime") => {
                        item.median_ns = m["median_ns"].as_f64();
                        item.mad_ns = m["mad_ns"].as_f64();
                        item.p99_ns = m["p99_ns"].as_f64();
                    }
                    Some("perfcnt") => {
                        item.instructions = m["instructions"].as_u64();
                        item.cycles = m["cycles"].as_u64();
                        item.cache_refs = m["cache_refs"].as_u64();
                    }
                    Some("callgrind") => {
                        item.ir = m["ir"].as_u64();
                        item.window = m["window"].as_u64();
                    }
                    Some("alloc") => {
                        item.allocs = m["allocs"].as_u64();
                        item.bytes = m["bytes"].as_u64();
                    }
                    Some("asyncexec") => {
                        item.polls = m["polls"].as_u64();
                        item.wakes = m["wakes"].as_u64();
                    }
                    Some("buildcost") => {
                        item.build_ms = m["build_ms"].as_u64();
                        item.size_bytes = m["size_bytes"].as_u64();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    run
}

/// Serialize a run into the baseline `items` shape.
pub fn run_to_items_value(run: &Run) -> Value {
    let mut items = serde_json::Map::new();
    for (id, m) in &run.items {
        let mut entry = json!({ "fingerprint": m.fingerprint, "covers": m.covers });
        if let Some(t) = m.tolerance_pct {
            entry["tolerance_pct"] = json!(t);
        }
        if let Some(v) = m.median_ns {
            entry["walltime"] = json!({
                "median_ns": v,
                "mad_ns": m.mad_ns.unwrap_or(0.0),
                "p99_ns": m.p99_ns.unwrap_or(0.0),
            });
            if !m.wall_rounds.is_empty() {
                entry["walltime"]["rounds"] = json!(m.wall_rounds);
            }
        }
        if let Some(i) = m.instructions {
            entry["perfcnt"] = json!({
                "instructions": i,
                "cycles": m.cycles.unwrap_or(0),
                "cache_refs": m.cache_refs.unwrap_or(0),
            });
        }
        if let Some(ir) = m.ir {
            entry["callgrind"] = json!({ "ir": ir });
            if let Some(window) = m.window {
                entry["callgrind"]["window"] = json!(window);
            }
        }
        if let (Some(a), Some(b)) = (m.allocs, m.bytes) {
            entry["alloc"] = json!({ "allocs": a, "bytes": b });
        }
        if let (Some(p), Some(w)) = (m.polls, m.wakes) {
            entry["asyncexec"] = json!({ "polls": p, "wakes": w });
        }
        if let (Some(ms), Some(sz)) = (m.build_ms, m.size_bytes) {
            entry["buildcost"] = json!({ "build_ms": ms, "size_bytes": sz });
        }
        if let Some(prefix) = &m.imported {
            entry["imported"] = json!(prefix);
        }
        if let Some(pkg) = &m.pkg {
            entry["pkg"] = json!(pkg);
        }
        items.insert(id.clone(), entry);
    }
    Value::Object(items)
}

/// Deserialize the baseline `items` shape back into a `Run`. The mirror of
/// `run_to_items_value`, for reusing a cached reference-shaped doc as a live
/// measurement instead of remeasuring it.
pub fn run_from_items_value(items: &Value) -> Run {
    let mut run = Run::default();
    let Some(map) = items.as_object() else {
        return run;
    };
    for (id, v) in map {
        run.items.insert(
            id.clone(),
            ItemMetrics {
                fingerprint: v["fingerprint"].as_str().unwrap_or("").to_string(),
                covers: v["covers"].as_str().unwrap_or("").to_string(),
                tolerance_pct: v["tolerance_pct"].as_f64(),
                median_ns: v["walltime"]["median_ns"].as_f64(),
                mad_ns: v["walltime"]["mad_ns"].as_f64(),
                p99_ns: v["walltime"]["p99_ns"].as_f64(),
                wall_rounds: v["walltime"]["rounds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_f64)
                    .collect(),
                instructions: v["perfcnt"]["instructions"].as_u64(),
                cycles: v["perfcnt"]["cycles"].as_u64(),
                cache_refs: v["perfcnt"]["cache_refs"].as_u64(),
                ir: v["callgrind"]["ir"].as_u64(),
                window: v["callgrind"]["window"].as_u64(),
                allocs: v["alloc"]["allocs"].as_u64(),
                bytes: v["alloc"]["bytes"].as_u64(),
                polls: v["asyncexec"]["polls"].as_u64(),
                wakes: v["asyncexec"]["wakes"].as_u64(),
                build_ms: v["buildcost"]["build_ms"].as_u64(),
                size_bytes: v["buildcost"]["size_bytes"].as_u64(),
                reused: None,
                imported: v["imported"].as_str().map(String::from),
                pkg: v["pkg"].as_str().map(String::from),
            },
        );
    }
    run
}

/// `run`'s items as a baseline stores them, each marked with what it reused.
/// Per item, since runs of several packages share one baseline file.
fn saved_items(run: &Run) -> Value {
    let mut items = run_to_items_value(run);
    if let Some(map) = items.as_object_mut() {
        for (id, item) in map {
            if let Some(reused) = run.items[id].reused.as_ref().or(run.reused.as_ref()) {
                item["reused"] = json!({ "from": reused.from, "metrics": reused.metrics });
            }
        }
    }
    items
}

/// Workspace root (parent of the workspace Cargo.toml), via cargo itself.
/// Resolved once: `git()` calls this on every invocation, and the root
/// cannot move under a running process.
pub fn workspace_root() -> io::Result<PathBuf> {
    static ROOT: OnceLock<io::Result<PathBuf>> = OnceLock::new();
    match ROOT.get_or_init(locate_workspace_root) {
        Ok(root) => Ok(root.clone()),
        Err(e) => Err(io::Error::other(e.to_string())),
    }
}

fn locate_workspace_root() -> io::Result<PathBuf> {
    let out = Command::new("cargo")
        .args(["locate-project", "--workspace", "--message-format", "plain"])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("cargo locate-project failed"));
    }
    let manifest = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(PathBuf::from(manifest)
        .parent()
        .expect("manifest path has a parent")
        .to_path_buf())
}

/// The cargo workspace root, else the git toplevel, else the current
/// directory: the root for commands that run outside a cargo project.
pub fn project_root() -> PathBuf {
    workspace_root()
        .ok()
        .or_else(|| {
            let out = Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn baseline_path(root: &Path, name: &str) -> PathBuf {
    root.join(".soothfast")
        .join("baselines")
        .join(format!("{name}.json"))
}

/// What a measurement run covered, deciding which stale baseline entries a
/// save may drop. One baseline file is shared by bench runs of several
/// packages plus buildcost runs, so each full run replaces only its own
/// slice; a filtered run updates in place and can leave stale ids behind
/// (gate's --filter exemption mirrors this).
pub enum SaveScope {
    /// Full bench run of one package (ids are package-qualified) — or of the
    /// whole workspace when no `-p` was given (`None` replaces every bench id).
    BenchFull(Option<String>),
    BenchFiltered(Option<String>),
    /// Buildcost run of one package (`None`: replace every buildcost entry).
    Buildcost(Option<String>),
    /// Import under `<prefix>`: replaces the items it marked with that prefix.
    Imported(String),
}

impl SaveScope {
    /// Scope of the run these args produced (what `--save-baseline` may
    /// replace) — shared by `measure` and `gate`.
    pub fn of(common: &CommonArgs) -> SaveScope {
        if common.backend.as_deref() == Some("buildcost") {
            SaveScope::Buildcost(common.pkg.clone())
        } else if common.filter.is_some() {
            SaveScope::BenchFiltered(common.pkg.clone())
        } else {
            SaveScope::BenchFull(common.pkg.clone())
        }
    }
}

/// First segment of a measured-item id (the package, `-` normalized to `_`).
pub fn id_pkg(id: &str) -> &str {
    id.split("::").next().unwrap_or(id)
}

fn is_imported(item: &Value) -> bool {
    item["imported"].is_string()
}

/// Persist a run as a named baseline.
pub fn save_baseline(name: &str, run: &Run, scope: SaveScope) -> io::Result<PathBuf> {
    save_baseline_in(&workspace_root()?, name, run, scope)
}

/// Persist a run as a named baseline under `root`.
pub fn save_baseline_in(
    root: &Path,
    name: &str,
    run: &Run,
    scope: SaveScope,
) -> io::Result<PathBuf> {
    let mut doc =
        load_baseline_in(root, name)?.unwrap_or_else(|| json!({ "version": 1, "items": {} }));
    let (foreign, mine) = match &scope {
        SaveScope::BenchFull(Some(pkg)) | SaveScope::BenchFiltered(Some(pkg)) => {
            let foreign = crate::ambiguity::foreign(&doc, pkg, run);
            let kept = crate::ambiguity::without(run, &foreign);
            (foreign, Some(kept))
        }
        _ => (BTreeSet::new(), None),
    };
    let owners_run = run;
    let run = mine.as_ref().unwrap_or(run);
    let mut new_items = saved_items(run);
    if let SaveScope::BenchFull(Some(pkg)) | SaveScope::BenchFiltered(Some(pkg)) = &scope {
        for item in new_items
            .as_object_mut()
            .into_iter()
            .flat_map(|m| m.values_mut())
        {
            item["saved_by"] = json!(pkg);
        }
    }
    let measured = match &scope {
        SaveScope::BenchFull(pkg) => Some(pkg.clone()),
        _ => None,
    };
    let prior = own_measured(&mut doc, &scope, owners_run, &foreign)?;
    let unix_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    doc["created_unix"] = json!(unix_now);
    if let Some(n) = run.noise_pct {
        doc["noise_pct"] = json!(n);
    }
    if let Some(b) = &run.build {
        doc["build"] = b.to_json();
    }
    if let (Some(map), Some(new_map)) = (doc["items"].as_object_mut(), new_items.as_object()) {
        // Full runs replace their scope: renamed/removed items must not
        // haunt the baseline (the gate fails on GONE entries).
        match scope {
            SaveScope::BenchFull(Some(_)) => map.retain(|k, _| !prior.contains(k)),
            SaveScope::BenchFull(None) => {
                map.retain(|k, v| is_imported(v) || k.starts_with("buildcost::"))
            }
            SaveScope::Buildcost(Some(pkg)) => map.retain(|k, _| {
                k.strip_prefix("buildcost::")
                    .is_none_or(|rest| rest.split("::").next() != Some(pkg.as_str()))
            }),
            SaveScope::Buildcost(None) => map.retain(|k, _| !k.starts_with("buildcost::")),
            SaveScope::Imported(prefix) => {
                map.retain(|_, v| v["imported"].as_str() != Some(prefix.as_str()))
            }
            SaveScope::BenchFiltered(_) => {}
        }
        for (k, v) in new_map {
            map.insert(k.clone(), v.clone());
        }
    }
    // Assertion verdicts ride along so renderers can show *verified* claims
    // ("proven O(n log n), zero-alloc") from produced data. Same scope rules
    // as items: a full run replaces its slice, keyed by the assertion's id.
    let replaced: Vec<String> = run.items.keys().chain(&prior).cloned().collect();
    let mut assertions: Vec<Value> = doc["assertions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| {
            a["id"]
                .as_str()
                .is_none_or(|id| !replaced.contains(&id.to_string()))
        })
        .collect();
    for a in &run.assertions {
        assertions.push(a.to_json());
    }
    doc["assertions"] = Value::Array(assertions);
    if let Some(pkg) = measured {
        crate::unmeasured::clear(&mut doc, pkg.as_deref());
    }

    crate::ambiguity::settle(&mut doc, &Default::default());
    write_baseline_in(root, name, &doc)?;
    prune_absorbed_accepts(root, run);
    Ok(baseline_path(root, name))
}

/// For a `-p` bench save, record in `doc` that the package owns what `run`
/// measured, and return what it owned before and no other package owns,
/// which a full save replaces. A filtered save adds to its owners and
/// replaces nothing beyond its own ids.
fn own_measured(
    doc: &mut Value,
    scope: &SaveScope,
    run: &Run,
    foreign: &BTreeSet<String>,
) -> io::Result<BTreeSet<String>> {
    let (pkg, full) = match scope {
        SaveScope::BenchFull(Some(pkg)) => (pkg, true),
        SaveScope::BenchFiltered(Some(pkg)) => (pkg, false),
        _ => return Ok(BTreeSet::new()),
    };
    let bench = |id: &str, v: &Value| !is_imported(v) && !id.starts_with("buildcost::");
    let prior: BTreeSet<String> = if full && doc.get(crate::unmeasured::OWNERS_KEY).is_none() {
        let legacy: BTreeSet<String> = doc["items"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(id, v)| bench(id, v))
            .map(|(id, _)| id.clone())
            .collect();
        if !legacy.is_empty() {
            println!(
                "baseline: it predates item ownership, so this save of {pkg} replaces every bench item in it; re-save the other packages"
            );
        }
        legacy
    } else {
        crate::unmeasured::owned_by(doc, pkg).into_iter().collect()
    };
    let mut owned: BTreeSet<String> = if full { BTreeSet::new() } else { prior.clone() };
    let norm = |p: &str| p.replace('-', "_");
    let registers = |m: &ItemMetrics| m.pkg.as_deref().is_some_and(|p| norm(p) == norm(pkg));
    owned.extend(
        run.items
            .iter()
            .filter(|(id, m)| m.imported.is_none() && !id.starts_with("buildcost::"))
            .filter(|(id, m)| !foreign.contains(*id) || registers(m))
            .map(|(id, _)| id.clone()),
    );
    let owned: Vec<String> = owned.into_iter().collect();
    let others = crate::unmeasured::owned_by_others(doc, pkg);
    crate::unmeasured::set_owners(doc, pkg, &owned);
    crate::ambiguity::record_registered(doc, pkg, run, full);
    if !full {
        return Ok(BTreeSet::new());
    }
    Ok(prior
        .into_iter()
        .filter(|id| !others.contains(id))
        .collect())
}

/// Write `doc` as baseline `name` under `root`.
pub fn write_baseline_in(root: &Path, name: &str, doc: &Value) -> io::Result<()> {
    let path = baseline_path(root, name);
    std::fs::create_dir_all(path.parent().expect("baseline path has a parent"))?;
    std::fs::write(&path, serde_json::to_string_pretty(doc)?)
}

/// Drop `soothfast-gate.lock` entries a fresh baseline has absorbed. Best
/// effort: a baseline save must never fail because of the lock file.
fn prune_absorbed_accepts(root: &Path, run: &Run) {
    let Ok(entries) = crate::gate_lock::read(root) else {
        return;
    };
    let (pruned, removed) = crate::gate_lock::prune_absorbed(&entries, run);
    if removed.is_empty() {
        return;
    }
    if crate::gate_lock::write(root, &pruned).is_ok() {
        for id in removed {
            println!("soothfast-gate.lock: {id} absorbed into the new baseline, entry removed");
        }
    }
}

/// Load a named baseline, if present.
pub fn load_baseline(name: &str) -> io::Result<Option<Value>> {
    load_baseline_in(&workspace_root()?, name)
}

/// Load a named baseline under `root`, if present.
pub fn load_baseline_in(root: &Path, name: &str) -> io::Result<Option<Value>> {
    let path = baseline_path(root, name);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&text)?))
}

/// Directory of a workspace package (parent of its Cargo.toml).
pub fn pkg_dir(pkg: &str) -> io::Result<PathBuf> {
    pkg_meta(pkg).map(|m| m.dir)
}

/// A workspace package's directory and manifest fields, which stand in as
/// spec metadata defaults when `soothfast.toml` doesn't override them.
pub struct PkgMeta {
    pub dir: PathBuf,
    pub version: String,
    pub description: Option<String>,
}

pub fn pkg_meta(pkg: &str) -> io::Result<PkgMeta> {
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .stdout(Stdio::piped())
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("cargo metadata failed"));
    }
    let meta: Value = serde_json::from_slice(&out.stdout)?;
    let p = meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["name"] == pkg)
        .ok_or_else(|| io::Error::other(format!("no workspace package named {pkg:?}")))?;
    let dir = p["manifest_path"]
        .as_str()
        .map(|m| {
            PathBuf::from(m)
                .parent()
                .expect("manifest has parent")
                .to_path_buf()
        })
        .ok_or_else(|| io::Error::other(format!("package {pkg:?} has no manifest path")))?;
    Ok(PkgMeta {
        dir,
        version: p["version"].as_str().unwrap_or("0.0.0").to_string(),
        description: p["description"].as_str().map(String::from),
    })
}

/// Which items rustdoc should emit.
///
/// The docs engine wants the public surface and nothing else. Spec
/// generation needs the opposite: route handlers are commonly not `pub`, and
/// rustdoc omits private items entirely, so their signatures — and the
/// private fields of types they reference — would simply be missing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
    /// Public API only.
    Public,
    /// Include private items.
    Private,
}

/// rustdoc JSON schema this build was developed against.
///
/// The format is explicitly unstable and changes between nightlies. A
/// different one still parses, but the surface it yields can differ, and a
/// derived artifact generated under one version will look "stale" under
/// another. Naming both numbers turns that into an explicable message.
pub(crate) const RUSTDOC_FORMAT_VERSION: u64 = 61;

/// Toolchain that builds rustdoc JSON, `SOOTHFAST_RUSTDOC_TOOLCHAIN` or
/// plain `nightly`.
///
/// Pin a dated nightly to keep committed artifacts (`llms.txt`, generated
/// specs) reproducible: on a floating `nightly` they are only valid until
/// rustdoc next changes its output.
fn rustdoc_toolchain() -> String {
    std::env::var("SOOTHFAST_RUSTDOC_TOOLCHAIN").unwrap_or_else(|_| "nightly".to_string())
}

/// Warn when rustdoc's JSON is not the version this build knows, once per
/// process: the same mismatch would otherwise repeat per documented crate.
fn warn_on_format_version(doc: &Value) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);

    let Some(found) = doc["format_version"].as_u64() else {
        return;
    };
    if found == RUSTDOC_FORMAT_VERSION || WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "warning: rustdoc JSON format_version {found}, expected {RUSTDOC_FORMAT_VERSION}. \
         The extracted surface may differ from the one a committed artifact was \
         generated with. Pin a nightly with SOOTHFAST_RUSTDOC_TOOLCHAIN to make it \
         reproducible."
    );
}

/// `pkg`'s own crate directory plus every workspace-local (path) dependency
/// it depends on, transitively — the source trees whose changes can alter
/// `pkg`'s rustdoc JSON without touching `pkg`'s own files (e.g. through a
/// re-exported type).
fn local_crate_dirs(pkg: &str, dir: Option<&Path>) -> io::Result<Vec<PathBuf>> {
    let mut cmd = Command::new("cargo");
    cmd.args(["metadata", "--format-version", "1"]);
    cmd.stdout(Stdio::piped());
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(io::Error::other("cargo metadata failed"));
    }
    let meta: Value = serde_json::from_slice(&out.stdout)?;
    let packages = meta["packages"].as_array().cloned().unwrap_or_default();

    let mut dir_of: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut local: BTreeSet<String> = BTreeSet::new();
    for p in &packages {
        let Some(id) = p["id"].as_str() else { continue };
        if let Some(cd) = p["manifest_path"]
            .as_str()
            .and_then(|m| Path::new(m).parent())
        {
            dir_of.insert(id.to_string(), cd.to_path_buf());
        }
        if p["source"].is_null() {
            local.insert(id.to_string());
        }
    }
    let Some(root_id) = packages
        .iter()
        .find(|p| p["name"] == pkg)
        .and_then(|p| p["id"].as_str())
        .map(str::to_string)
    else {
        return Ok(Vec::new());
    };
    let deps_of: BTreeMap<String, Vec<String>> = meta["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            let id = n["id"].as_str()?.to_string();
            let deps = n["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|d| d.as_str().map(str::to_string))
                .collect();
            Some((id, deps))
        })
        .collect();

    let mut seen = BTreeSet::new();
    let mut stack = vec![root_id];
    let mut dirs = Vec::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if local.contains(&id)
            && let Some(d) = dir_of.get(&id)
        {
            dirs.push(d.clone());
        }
        stack.extend(deps_of.get(&id).cloned().into_iter().flatten());
    }
    Ok(dirs)
}

/// Build rustdoc JSON for a package (nightly-only job) and load it.
/// `dir` runs the build in another worktree; JSON lands in its target/doc.
///
/// Reuses a stored extraction of the same inputs (see `rustdocstore`) instead
/// of re-running `cargo rustdoc`: rustdoc's JSON backend re-serializes on every
/// invocation even when the compilation is fully cached, so repeat calls for
/// one package, and the same sources in another run, would pay for it again.
pub fn rustdoc_json_in(
    pkg: &str,
    dir: Option<&Path>,
    features: Option<&str>,
    visibility: Visibility,
) -> io::Result<(Value, PathBuf)> {
    let toolchain = rustdoc_toolchain();
    let root = match dir {
        Some(d) => d.to_path_buf(),
        None => workspace_root()?,
    };
    // Honor CARGO_TARGET_DIR / build.target-dir: a hardcoded target/ would
    // silently read a STALE json and validate docs against an old surface.
    let json_path = target_dir(dir)?
        .join("doc")
        .join(format!("{}.json", pkg.replace('-', "_")));

    let store_key = || rustdoc_store_key(pkg, dir, &root, features, visibility, &toolchain);
    let key = store_key();
    if let Some(doc) = key.as_deref().and_then(|k| rustdocstore::load(k, &root)) {
        warn_on_format_version(&doc);
        return Ok((doc, root));
    }
    let dep_info = json_path.with_extension("soothfast.d");
    let _ = std::fs::remove_file(&dep_info);
    let started = std::time::SystemTime::now();

    let build = |dep_info: Option<&Path>| {
        let cmd = rustdoc_command(&toolchain, pkg, dir, features, visibility, dep_info);
        timed(&format!("cargo rustdoc {pkg}"), || run_teeing_stderr(cmd))
    };
    let mut built = build(Some(&dep_info))?;
    let mut tracked = true;
    if !built.status.success() && rejects_tracking_flags(&built.stderr) {
        tracked = false;
        built = build(None)?;
    }
    let failed = |built: &Built| {
        let hint = match toolchain_trouble(&built.stderr, &toolchain) {
            Some(ToolchainTrouble::Missing) => {
                format!(" (is the {toolchain} toolchain installed?)")
            }
            Some(ToolchainTrouble::Components) => {
                format!(" ({toolchain} could not be installed with its components)")
            }
            None => String::new(),
        };
        io::Error::other(format!(
            "cargo +{toolchain} rustdoc failed with {}{hint}",
            built.status
        ))
    };
    if !built.status.success() {
        return Err(failed(&built));
    }
    // With `--emit=dep-info` rustdoc writes nothing else, so the JSON comes
    // from a second run without it.
    if tracked {
        let plain = build(None)?;
        if !plain.status.success() {
            return Err(failed(&plain));
        }
    }
    let text = std::fs::read_to_string(&json_path)
        .map_err(|e| io::Error::other(format!("cannot read {}: {e}", json_path.display())))?;
    let doc: Value = timed(&format!("parse rustdoc json {pkg}"), || {
        serde_json::from_str(&text)
    })?;
    warn_on_format_version(&doc);
    if let (Some(key), true) = (key, tracked) {
        let inputs = rustdoc_inputs::collect(&root, &dep_info, &built.stdout);
        rustdocstore::store(&key, store_key(), &root, &inputs, started, &json_path);
    }
    Ok((doc, root))
}

/// The store key `rustdoc_json_in` looks `pkg` up under in the workspace.
pub fn rustdoc_key(pkg: &str, features: Option<&str>, visibility: Visibility) -> Option<String> {
    let root = workspace_root().ok()?;
    rustdoc_store_key(pkg, None, &root, features, visibility, &rustdoc_toolchain())
}

/// Whether the store holds the rustdoc JSON under `key`.
pub fn rustdoc_holds(key: &str) -> bool {
    workspace_root().is_ok_and(|root| rustdocstore::holds(key, &root))
}

/// `cargo rustdoc` for `pkg`, recording what it read in `dep_info` when given.
fn rustdoc_command(
    toolchain: &str,
    pkg: &str,
    dir: Option<&Path>,
    features: Option<&str>,
    visibility: Visibility,
    dep_info: Option<&Path>,
) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.args([&format!("+{toolchain}"), "rustdoc", "-p", pkg, "--lib"]);
    if dep_info.is_some() {
        cmd.arg("--message-format=json-render-diagnostics");
    }
    // Feature-gated items are invisible to the surface unless enabled.
    if let Some(f) = features {
        cmd.args(["--features", f]);
    }
    cmd.args(["--", "--output-format", "json", "-Zunstable-options"]);
    if let Some(path) = dep_info {
        cmd.arg(format!("--emit=dep-info={}", path.display()));
    }
    if visibility == Visibility::Private {
        cmd.arg("--document-private-items");
    }
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    cmd
}

/// Whether `stderr` says the toolchain refused the flags that record what a
/// rustdoc build read, as opposed to the documented crate failing to build.
fn rejects_tracking_flags(stderr: &str) -> bool {
    let e = stderr.to_lowercase();
    e.contains("invalid message format")
        || (e.contains("emit")
            && (e.contains("unrecognized option") || e.contains("unexpected argument")))
}

/// Why a `cargo +TOOLCHAIN` failure is the toolchain's fault rather than the
/// documented crate's.
#[derive(Debug, PartialEq)]
enum ToolchainTrouble {
    /// Absent, or rustup could not fetch it.
    Missing,
    /// rustup has the toolchain but not every component it must install with it.
    Components,
}

/// What a `cargo +TOOLCHAIN` failure says is wrong with `toolchain`, from
/// rustup or from a `cargo` that is no rustup proxy. A component missing from
/// an installed toolchain is not its fault, and neither is a build script's
/// error, whether cargo replays it under `--- stderr` or as `error: PKG@VER:`.
fn toolchain_trouble(stderr: &str, toolchain: &str) -> Option<ToolchainTrouble> {
    let tc = toolchain.to_lowercase();
    let channel = tc.split_once('-').map_or(tc.as_str(), |(c, _)| c);
    let quoted = format!("'{tc}");
    let dist = format!("channel-rust-{channel}");
    let unavailable = format!("is unavailable for download for channel {quoted}");
    let no_release = format!("no release found for {quoted}");
    let no_command = format!("no such command: `+{tc}");
    let fetch = [
        "could not download",
        "failed to install",
        "error downloading",
    ];
    let from_build_script = |l: &str| {
        l.strip_prefix("error:")
            .and_then(|rest| rest.split_whitespace().next())
            .is_some_and(|word| word.ends_with(':') && word.contains('@'))
    };
    let named_missing = |l: &str| {
        !l.contains("component")
            && l.find(&quoted).is_some_and(|at| {
                l[at + quoted.len()..]
                    .split_once(' ')
                    .is_some_and(|(word, after)| {
                        word.ends_with('\'') && after.starts_with("is not install")
                    })
            })
    };
    let mut lines = stderr
        .lines()
        .map(|l| l.trim().to_lowercase())
        .take_while(|l| !l.starts_with("--- stderr") && !l.starts_with("--- stdout"))
        .filter(|l| l.starts_with("error:") && !from_build_script(l));
    lines.find_map(|l| {
        if l.contains(&unavailable) {
            return Some(ToolchainTrouble::Components);
        }
        let fetch_failed =
            fetch.iter().any(|p| l.contains(p)) && (l.contains(&quoted) || l.contains(&dist));
        (named_missing(&l) || fetch_failed || l.contains(&no_release) || l.contains(&no_command))
            .then_some(ToolchainTrouble::Missing)
    })
}

struct Built {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: String,
}

/// Run `cmd`, passing its stderr through as it arrives and keeping a copy.
fn run_teeing_stderr(mut cmd: Command) -> io::Result<Built> {
    use std::io::{Read, Write};
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut err = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("no stderr"))?;
    let tee = std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut chunk = [0u8; 8192];
        while let Ok(n) = err.read(&mut chunk) {
            if n == 0 {
                break;
            }
            let _ = io::stderr().write_all(&chunk[..n]);
            kept.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8_lossy(&kept).into_owned()
    });
    let out = child.wait_with_output()?;
    let stderr = tee.join().unwrap_or_default();
    Ok(Built {
        status: out.status,
        stdout: out.stdout,
        stderr,
    })
}

/// `local_crate_dirs`, resolved once per package and tree.
pub(crate) fn crate_dirs(pkg: &str, dir: Option<&Path>) -> Option<Vec<PathBuf>> {
    type Dirs = BTreeMap<(String, Option<PathBuf>), Vec<PathBuf>>;
    static DIRS: Mutex<Dirs> = Mutex::new(BTreeMap::new());
    let at = (pkg.to_string(), dir.map(Path::to_path_buf));
    let known = DIRS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&at)
        .cloned();
    if known.is_some() {
        return known;
    }
    let dirs = local_crate_dirs(pkg, dir).ok()?;
    DIRS.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(at, dirs.clone());
    Some(dirs)
}

fn rustdoc_store_key(
    pkg: &str,
    dir: Option<&Path>,
    root: &Path,
    features: Option<&str>,
    visibility: Visibility,
    toolchain: &str,
) -> Option<String> {
    let crate_dirs = crate_dirs(pkg, dir)?;
    let rustc = store::rustc_version(Some(toolchain), root)?;
    let env = store::build_env(&[]);
    let request = rustdocstore::Request {
        pkg,
        features,
        visibility,
        rustc: &rustc,
        env: &env,
    };
    rustdocstore::key(root, &crate_dirs, &request)
}

/// Check out the merge-base of HEAD and `refname` under
/// `.soothfast/worktrees/<sha>` and run `f` against it.
///
/// The checkout is kept between runs. Re-creating it would rewrite every
/// file's mtime, and cargo's freshness for path dependencies is mtime-based,
/// so the shared worktree target dir would rebuild every workspace member.
pub fn with_merge_base_worktree<T>(
    refname: &str,
    f: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    let sha = git(&["merge-base", "HEAD", refname]).map_err(|e| e.to_string())?;
    let root = workspace_root().map_err(|e| e.to_string())?;
    let dir = root.join(".soothfast").join("worktrees");
    let wt = dir.join(&sha);
    let wt_str = wt
        .to_str()
        .ok_or_else(|| "worktree path is not UTF-8".to_string())?;

    forget_missing_worktree(&wt);
    discard_worktrees_except(&dir, &sha);

    if is_worktree_root(&wt) {
        git(&["-C", wt_str, "checkout", "--force", "--detach", &sha]).map_err(|e| e.to_string())?;
        git(&["-C", wt_str, "clean", "-fd"]).map_err(|e| e.to_string())?;
    } else {
        let _ = std::fs::remove_dir_all(&wt);
        git(&["worktree", "add", "--detach", wt_str, &sha]).map_err(|e| e.to_string())?;
    }
    verify_worktree(wt_str, &sha)?;
    f(&wt)
}

/// Whether `wt` is itself the root of a linked worktree. Testing for a
/// reachable git dir would accept any directory inside the repository and
/// send a later `checkout` at the main checkout instead.
fn is_worktree_root(wt: &Path) -> bool {
    if !wt.join(".git").is_file() {
        return false;
    }
    let Some(wt_str) = wt.to_str() else {
        return false;
    };
    git(&["-C", wt_str, "rev-parse", "--show-toplevel"])
        .is_ok_and(|top| same_path(Path::new(&top), wt))
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The reference side is only worth measuring if the tree it measures is the
/// merge-base, so a reused checkout has to prove it still is.
fn verify_worktree(wt: &str, sha: &str) -> Result<(), String> {
    let head = git(&["-C", wt, "rev-parse", "HEAD"]).map_err(|e| e.to_string())?;
    if head != sha {
        return Err(format!("worktree {wt} is at {head}, not merge-base {sha}"));
    }
    match git(&["-C", wt, "status", "--porcelain"]) {
        Ok(out) if out.is_empty() => Ok(()),
        Ok(_) => Err(format!("worktree {wt} has local changes; delete it")),
        Err(e) => Err(e.to_string()),
    }
}

/// Drop checkouts of every other merge-base, so a rebased branch does not
/// leave one behind per base it has had.
fn discard_worktrees_except(dir: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name() == std::ffi::OsStr::new(keep) {
            continue;
        }
        if let Some(path) = entry.path().to_str() {
            let _ = git(&["worktree", "remove", "--force", path]);
        }
        let _ = std::fs::remove_dir_all(entry.path());
        forget_missing_worktree(&entry.path());
    }
}

/// Drop the registration of `wt` when its directory is gone. A deleted
/// worktree stays registered and `add` refuses the path. Never `git worktree
/// prune`: that drops every other worktree whose directory this process
/// cannot see, such as one on an unmounted drive or outside a container.
pub fn forget_missing_worktree(wt: &Path) {
    if let Ok(root) = workspace_root() {
        forget_missing_worktree_in(&root, wt);
    }
}

fn forget_missing_worktree_in(repo: &Path, wt: &Path) {
    if wt.exists() {
        return;
    }
    if let Some(path) = wt.to_str() {
        let _ = git_in(repo, &["worktree", "remove", "--force", "--force", path]);
    }
}

/// One `[[package]]` entry from a Cargo.lock.
struct LockPkg {
    name: String,
    version: String,
    /// Registry entries carry a `source` line; path dependencies don't and
    /// cannot be re-pinned.
    registry: bool,
}

fn lock_packages(lock: &str) -> Vec<LockPkg> {
    lock.split("[[package]]")
        .skip(1)
        .filter_map(|block| {
            let field = |key: &str| {
                block
                    .lines()
                    .find_map(|l| l.strip_prefix(key))
                    .map(|v| v.trim().trim_matches('"').to_string())
            };
            Some(LockPkg {
                name: field("name = ")?,
                version: field("version = ")?,
                registry: block.lines().any(|l| l.starts_with("source = ")),
            })
        })
        .collect()
}

/// Soothfast-family registry entries whose worktree version differs from
/// HEAD's: (name, worktree version, HEAD version).
fn harness_mismatches(head_lock: &str, wt_lock: &str) -> Vec<(String, String, String)> {
    let head: BTreeMap<String, LockPkg> = lock_packages(head_lock)
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect();
    lock_packages(wt_lock)
        .into_iter()
        .filter(|p| p.registry && (p.name == "soothfast" || p.name.starts_with("soothfast-")))
        .filter_map(|p| {
            let h = head.get(&p.name).filter(|h| h.registry)?;
            (h.version != p.version).then(|| (p.name, p.version, h.version.clone()))
        })
        .collect()
}

/// HEAD's locked soothfast versions, as `name@version` pairs. The reference
/// side is pinned to these before it is built, so they decide what a
/// reference measurement means.
pub fn harness_versions() -> String {
    let Ok(root) = workspace_root() else {
        return String::new();
    };
    let Ok(lock) = std::fs::read_to_string(root.join("Cargo.lock")) else {
        return String::new();
    };
    let mut out: Vec<String> = lock_packages(&lock)
        .into_iter()
        .filter(|p| p.name == "soothfast" || p.name.starts_with("soothfast-"))
        .map(|p| format!("{}@{}", p.name, p.version))
        .collect();
    out.sort();
    out.join(",")
}

/// Whether the working tree has no uncommitted changes, so HEAD's SHA names
/// what was actually measured.
pub fn tree_is_clean() -> bool {
    git(&["status", "--porcelain"]).is_ok_and(|s| s.trim().is_empty())
}

/// Whether the reference side ended up embedding HEAD's measurement harness.
/// A reference built against another harness measures a different protocol,
/// so the outcome travels with the run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarnessSync {
    /// Crates cargo would not move, each spelled `name 0.2.0 -> 0.3.1`.
    unpinned: Vec<String>,
    /// The callgrind methods of the reference and of HEAD, when they differ
    /// or the reference's is unknown.
    method: Option<(String, String)>,
}

impl HarnessSync {
    /// Rebuild an outcome from the crate labels a stored run kept.
    pub fn from_unpinned(unpinned: Vec<String>) -> Self {
        HarnessSync {
            unpinned,
            method: None,
        }
    }

    /// The crates left at a version of their own, empty when the pin held.
    pub fn unpinned(&self) -> &[String] {
        &self.unpinned
    }

    /// This outcome with the callgrind methods of the two sides.
    pub fn with_method(self, base: &str, head: &str) -> Self {
        HarnessSync {
            method: Some((base.into(), head.into())),
            ..self
        }
    }

    /// The stored form of [`HarnessSync::with_method`]: `[base, head]`.
    pub fn method(&self) -> Option<[&str; 2]> {
        self.method.as_ref().map(|(b, h)| [b.as_str(), h.as_str()])
    }

    /// The method difference, spelled for a reader, when there is one.
    pub fn method_note(&self) -> Option<String> {
        self.method().map(|[base, head]| {
            format!("base measured with callgrind method {base}, head with {head}")
        })
    }

    /// Whether the two sides embed different measurement harnesses.
    pub fn is_mismatched(&self) -> bool {
        !self.unpinned.is_empty() || self.method.is_some()
    }
}

fn sync_outcome(mismatches: &[(String, String, String)]) -> HarnessSync {
    HarnessSync::from_unpinned(
        mismatches
            .iter()
            .map(|(name, from, to)| format!("{name} {from} -> {to}"))
            .collect(),
    )
}

/// Pin the merge-base worktree's soothfast crates to HEAD's locked versions
/// so the reference bench binary embeds the same measurement harness. A pin
/// the worktree's own requirement excludes is retried after rewriting that
/// requirement; anything still unpinned comes back as `Mismatched` for the
/// caller to put in front of the verdict.
pub fn sync_harness_versions(wt: &Path) -> Result<HarnessSync, String> {
    let head_path = workspace_root()
        .map_err(|e| e.to_string())?
        .join("Cargo.lock");
    let Ok(head) = std::fs::read_to_string(&head_path) else {
        return Ok(HarnessSync::default());
    };
    // One crate per pass, re-reading the lock: pinning the facade drags its
    // family along, and already-moved entries must not be warned about.
    for _ in 0..16 {
        let Ok(base) = std::fs::read_to_string(wt.join("Cargo.lock")) else {
            return Ok(HarnessSync::default());
        };
        let mismatches = harness_mismatches(&head, &base);
        let Some((name, from, to)) = mismatches.first().cloned() else {
            return Ok(HarnessSync::default());
        };
        println!(
            "gate: pinning {name} {from} -> {to} in the merge-base worktree (harness must match HEAD)"
        );
        let mut refused = pin_precise(wt, &name, &from, &to)?;
        // A caret-incompatible bump is refused by the merge-base's own
        // requirement. The worktree is disposable, so widen it and retry.
        if refused.as_deref().is_some_and(is_requirement_conflict)
            && relax_harness_requirements(wt, &mismatches)?
        {
            println!("gate: widened the merge-base's soothfast requirements to retry the pin");
            refused = pin_precise(wt, &name, &from, &to)?;
        }
        if let Some(stderr) = refused {
            eprintln!(
                "WARN: could not pin {name} ({}); the reference keeps its own harness — deltas may reflect the harness change itself",
                stderr.trim().lines().last().unwrap_or("no error output")
            );
            return Ok(sync_outcome(&mismatches));
        }
    }
    let remaining = std::fs::read_to_string(wt.join("Cargo.lock"))
        .map(|base| harness_mismatches(&head, &base))
        .unwrap_or_default();
    Ok(sync_outcome(&remaining))
}

/// Cargo's precise pin, run in the worktree. `Ok(None)` when it landed,
/// `Ok(Some(stderr))` when cargo refused it.
fn pin_precise(wt: &Path, name: &str, from: &str, to: &str) -> Result<Option<String>, String> {
    let out = Command::new("cargo")
        .args(["update", "-p", &format!("{name}@{from}"), "--precise", to])
        .current_dir(wt)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&out.stderr).into_owned()))
}

/// Whether cargo refused the pin because a requirement excludes the wanted
/// version, rather than because the index or network was unreachable.
fn is_requirement_conflict(stderr: &str) -> bool {
    stderr.contains("didn't match")
        || stderr.contains("does not match")
        || stderr.contains("failed to select a version")
}

/// Point the worktree's soothfast-family requirements at the versions HEAD
/// locks, so the pin above is no longer refused. Returns whether any
/// manifest changed.
fn relax_harness_requirements(
    wt: &Path,
    mismatches: &[(String, String, String)],
) -> Result<bool, String> {
    let wanted: BTreeMap<&str, &str> = mismatches
        .iter()
        .map(|(name, _, to)| (name.as_str(), to.as_str()))
        .collect();
    let mut changed = false;
    for path in manifests(wt) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(next) = retarget_requirements(&text, &wanted) else {
            continue;
        };
        std::fs::write(&path, next).map_err(|e| format!("{}: {e}", path.display()))?;
        changed = true;
    }
    Ok(changed)
}

/// Every `Cargo.toml` under `dir`. `target`/`.git` hold nothing the
/// resolver reads.
fn manifests(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            out.extend(manifests(&path));
        } else if name == "Cargo.toml" {
            out.push(path);
        }
    }
    out
}

/// Rewrite the `version` requirement of each dependency `wanted` names, in
/// one manifest's text. `None` when nothing matched. Only the requirement
/// string moves: re-specifying the dependency would drop `features`,
/// `optional` or `default-features` beside it.
fn retarget_requirements(text: &str, wanted: &BTreeMap<&str, &str>) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut table = String::new();
    let mut changed = false;
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches('\n').trim_end_matches('\r');
        let ending = &raw[line.len()..];
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            table = trimmed
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .to_string();
            out.push_str(raw);
            continue;
        }
        let rewritten = match dep_table_name(&table) {
            Some(name) => version_key(line)
                .zip(wanted.get(name))
                .and_then(|(at, to)| retarget_line(line, at, to)),
            None => dep_line(line, &table, wanted).and_then(|(at, to)| retarget_line(line, at, to)),
        };
        match rewritten {
            Some(next) => {
                out.push_str(&next);
                out.push_str(ending);
                changed = true;
            }
            None => out.push_str(raw),
        }
    }
    changed.then_some(out)
}

/// Where a `version = "..."` line's value starts.
fn version_key(line: &str) -> Option<usize> {
    let (key, rest) = line.split_once('=')?;
    (key.trim().trim_matches('"') == "version").then(|| line.len() - rest.len())
}

/// The dependency a `[…dependencies.NAME]` header names.
fn dep_table_name(table: &str) -> Option<&str> {
    let (parent, name) = table.rsplit_once('.')?;
    is_dep_table(parent).then_some(name)
}

fn is_dep_table(table: &str) -> bool {
    matches!(
        table.rsplit('.').next(),
        Some("dependencies" | "dev-dependencies" | "build-dependencies")
    )
}

/// Where a dependency line's requirement starts, and what it should become.
/// A path or git dependency is left alone: its version is not what the
/// resolver picks.
fn dep_line<'a>(
    line: &str,
    table: &str,
    wanted: &BTreeMap<&str, &'a str>,
) -> Option<(usize, &'a str)> {
    if !is_dep_table(table) {
        return None;
    }
    let (key, rest) = line.split_once('=')?;
    let to = wanted.get(key.trim().trim_matches('"'))?;
    let rest_at = line.len() - rest.len();
    let value = rest.trim_start();
    if value.starts_with('"') {
        return Some((rest_at, to));
    }
    if !value.starts_with('{') || value.contains("path =") || value.contains("git =") {
        return None;
    }
    let version_at = rest_at + rest.find("version")?;
    Some((version_at, to))
}

/// Replace the first quoted string at or after `from` with `to`, leaving
/// the rest of the line (other keys, a trailing comment) untouched.
fn retarget_line(line: &str, from: usize, to: &str) -> Option<String> {
    let open = from + line.get(from..)?.find('"')?;
    let close = open + 1 + line.get(open + 1..)?.find('"')?;
    (&line[open + 1..close] != to)
        .then(|| format!("{}\"{to}\"{}", &line[..open], &line[close + 1..]))
}

/// Copy an untracked `.cargo/config.toml` (or legacy `.cargo/config`) into
/// the worktree. `git worktree add` only checks out tracked content, so a
/// local file that shapes rustflags would otherwise silently vanish from
/// the reference build. A tracked config is left alone: it's the commit's
/// real content, which may differ on purpose.
pub fn sync_untracked_cargo_config(wt: &Path) -> Result<(), String> {
    let root = workspace_root().map_err(|e| e.to_string())?;
    for name in [".cargo/config.toml", ".cargo/config"] {
        let src = root.join(name);
        if !src.exists() || git(&["ls-files", "--error-unmatch", name]).is_ok() {
            continue;
        }
        let dst = wt.join(name);
        std::fs::create_dir_all(dst.parent().expect("config path has a parent"))
            .map_err(|e| e.to_string())?;
        std::fs::copy(&src, &dst).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Effective cargo target directory (respects CARGO_TARGET_DIR and
/// .cargo/config.toml), optionally for a build rooted in another worktree.
pub fn target_dir(dir: Option<&Path>) -> io::Result<PathBuf> {
    let mut cmd = Command::new("cargo");
    cmd.args(["metadata", "--no-deps", "--format-version", "1"]);
    cmd.stdout(Stdio::piped());
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(io::Error::other("cargo metadata failed"));
    }
    let meta: Value = serde_json::from_slice(&out.stdout)?;
    meta["target_directory"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("cargo metadata: no target_directory"))
}

/// Persistent target dir for merge-base worktree bench builds: a sibling
/// inside the parent's target dir, surviving the per-run worktree churn so
/// dependencies compile once per machine instead of once per gate. Sharing
/// the parent target dir itself is unsound: a workspace member hashes to
/// the same `-C metadata` from either path, so head and base leaf artifacts
/// would collide (and mtime races silently serve the wrong side's binary).
pub fn worktree_target_dir() -> io::Result<PathBuf> {
    Ok(target_dir(None)?.join("soothfast-worktree"))
}

/// Buildcost's own target dir. `cargo clean -p` drops every artifact of the
/// package under the profile dir whatever its hash, so a dir shared with the
/// bench gate would lose the gate's copies on each buildcost leg.
pub fn buildcost_target_dir() -> io::Result<PathBuf> {
    Ok(target_dir(None)?.join("soothfast-buildcost"))
}

/// The merge-base side of [`buildcost_target_dir`], kept beside the parent
/// target dir so it outlives the worktree the sources are checked out in.
/// Separate from the head dir for the reason [`worktree_target_dir`] gives:
/// a member hashes to the same `-C metadata` from either checkout.
pub fn buildcost_base_target_dir() -> io::Result<PathBuf> {
    Ok(target_dir(None)?.join("soothfast-buildcost-base"))
}

/// Run a git command in the workspace root, returning trimmed stdout.
pub fn git(args: &[&str]) -> io::Result<String> {
    let root = workspace_root()?;
    git_in(&root, args)
}

/// Run a git command in `dir`, returning trimmed stdout.
pub fn git_in(dir: &Path, args: &[&str]) -> io::Result<String> {
    let out = Command::new("git").args(args).current_dir(dir).output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        CommonArgs, Guest, HostEnv, ItemMetrics, Reused, Run, SaveScope, Shard,
        forget_missing_worktree_in, git_in, harness_mismatches, run_from_items_value,
        run_to_items_value, saved_items,
    };

    #[test]
    fn only_our_missing_worktree_is_forgotten() {
        let root = std::env::temp_dir().join(format!("soothfast-forget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let (ours, foreign) = (root.join("ours"), root.join("foreign"));
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| git_in(&repo, args).unwrap();
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ]);
        for wt in [&ours, &foreign] {
            git(&["worktree", "add", "-q", "--detach", wt.to_str().unwrap()]);
            std::fs::remove_dir_all(wt).unwrap();
        }
        forget_missing_worktree_in(&repo, &ours);
        let listing = git(&["worktree", "list", "--porcelain"]);
        assert!(!listing.contains("ours"), "{listing}");
        assert!(listing.contains("foreign"), "{listing}");
    }

    #[test]
    fn a_shard_parses_from_one_and_rejects_out_of_range() {
        assert_eq!(Shard::parse("2/4"), Ok(Shard { index: 2, count: 4 }));
        for bad in ["0/4", "5/4", "1", "a/b", "1/0"] {
            assert!(Shard::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_item_belongs_to_exactly_one_shard() {
        let shards: Vec<Shard> = (1..=4).map(|index| Shard { index, count: 4 }).collect();
        for n in 0..200 {
            let id = format!("pkg::item_{n}");
            assert_eq!(shards.iter().filter(|s| s.owns(&id)).count(), 1, "{id}");
        }
    }

    #[test]
    fn a_saved_item_says_which_metrics_it_reused() {
        let mut run = Run::default();
        run.items.insert("pkg::a".into(), ItemMetrics::default());
        assert!(saved_items(&run)["pkg::a"].get("reused").is_none());
        run.reused = Some(Reused {
            from: "binary 0123456789abcdef".into(),
            metrics: vec!["callgrind.ir"],
        });
        let item = &saved_items(&run)["pkg::a"];
        assert_eq!(item["reused"]["from"], "binary 0123456789abcdef");
        assert_eq!(
            item["reused"]["metrics"],
            serde_json::json!(["callgrind.ir"])
        );
    }

    #[test]
    fn executed_records_are_kept_only_under_callgrind() {
        let executed = serde_json::json!({
            "type": "executed", "id": "pkg::a", "functions": [["/bin/bench", "f"]],
        });
        let under = |backend: &str| {
            super::collect(&[
                serde_json::json!({ "type": "env", "gating_backend": backend }),
                executed.clone(),
            ])
        };
        assert_eq!(
            under("callgrind").executed["pkg::a"],
            [("/bin/bench".to_string(), "f".to_string())]
        );
        assert!(under("perfcnt").executed.is_empty());
    }

    #[test]
    fn a_harness_advertises_item_reuse_in_its_env_line() {
        let line = |v: serde_json::Value| HostEnv::from_line(&v).map(|e| e.item_reuse);
        assert_eq!(
            line(serde_json::json!({ "gating_backend": "callgrind", "item_reuse": true })),
            Some(true)
        );
        assert_eq!(
            line(serde_json::json!({ "gating_backend": "callgrind" })),
            Some(false)
        );
    }

    #[test]
    fn a_saved_item_carries_its_own_reuse_marker() {
        let mut run = Run::default();
        run.items.insert("pkg::a".into(), ItemMetrics::default());
        run.items.insert(
            "pkg::b".into(),
            ItemMetrics {
                reused: Some(Reused {
                    from: "item cache".into(),
                    metrics: vec!["callgrind.ir"],
                }),
                ..ItemMetrics::default()
            },
        );
        let items = saved_items(&run);
        assert!(items["pkg::a"].get("reused").is_none());
        assert_eq!(items["pkg::b"]["reused"]["from"], "item cache");
    }

    #[test]
    fn a_harness_without_a_named_method_measures_two_process() {
        let line = serde_json::json!({ "type": "env", "gating_backend": "callgrind" });
        assert_eq!(
            HostEnv::from_line(&line)
                .map(|e| e.method().to_string())
                .as_deref(),
            Some("two-process")
        );
    }

    #[test]
    fn a_method_difference_mismatches_the_harness_without_a_pin() {
        let sync = super::HarnessSync::default().with_method("two-process", "one-process");
        assert!(sync.is_mismatched());
        assert!(sync.unpinned().is_empty());
        assert_eq!(
            sync.method_note().as_deref(),
            Some("base measured with callgrind method two-process, head with one-process")
        );
    }

    #[test]
    fn an_env_line_reads_its_guest_only_when_complete() {
        let full = serde_json::json!({
            "type": "env", "gating_backend": "callgrind",
            "guest_cpu": "0123456789abcdef", "guest_glibc": "2.39",
            "guest_libc": "fedcba9876543210", "valgrind": "valgrind-3.22.0",
            "callgrind_method": "one-process",
        });
        assert_eq!(
            HostEnv::from_line(&full),
            Some(HostEnv {
                gating_backend: "callgrind".into(),
                guest: Some(Guest {
                    cpu: "0123456789abcdef".into(),
                    glibc: "2.39".into(),
                    libc: "fedcba9876543210".into(),
                    valgrind: "valgrind-3.22.0".into(),
                }),
                callgrind_method: Some("one-process".into()),
                item_reuse: false,
            })
        );
        let partial = serde_json::json!({
            "type": "env", "gating_backend": "callgrind", "guest_cpu": "0123456789abcdef",
        });
        assert_eq!(HostEnv::from_line(&partial).and_then(|e| e.guest), None);
        assert_eq!(
            HostEnv::from_line(&serde_json::json!({ "type": "env" })),
            None
        );
    }

    #[test]
    fn run_from_items_value_round_trips_run_to_items_value() {
        let mut run = Run::default();
        run.items.insert(
            "pkg::bench".into(),
            ItemMetrics {
                fingerprint: "fp".into(),
                covers: "pkg::covered".into(),
                median_ns: Some(100.0),
                mad_ns: Some(1.0),
                p99_ns: Some(120.0),
                wall_rounds: vec![95.0, 105.0],
                instructions: Some(500),
                cycles: Some(600),
                cache_refs: Some(10),
                ir: Some(700),
                window: Some(10),
                allocs: Some(2),
                bytes: Some(64),
                polls: Some(3),
                wakes: Some(3),
                tolerance_pct: Some(8.0),
                build_ms: None,
                size_bytes: None,
                reused: None,
                imported: Some("jmh".into()),
                pkg: Some("pkg".into()),
            },
        );
        let round_tripped = run_from_items_value(&run_to_items_value(&run));
        let original = &run.items["pkg::bench"];
        let back = &round_tripped.items["pkg::bench"];
        assert_eq!(back.fingerprint, original.fingerprint);
        assert_eq!(back.covers, original.covers);
        assert_eq!(back.median_ns, original.median_ns);
        assert_eq!(back.mad_ns, original.mad_ns);
        assert_eq!(back.p99_ns, original.p99_ns);
        assert_eq!(back.imported, original.imported);
        assert_eq!(back.pkg, original.pkg);
        assert_eq!(back.wall_rounds, original.wall_rounds);
        assert_eq!(back.instructions, original.instructions);
        assert_eq!(back.cycles, original.cycles);
        assert_eq!(back.cache_refs, original.cache_refs);
        assert_eq!(back.ir, original.ir);
        assert_eq!(back.allocs, original.allocs);
        assert_eq!(back.bytes, original.bytes);
        assert_eq!(back.polls, original.polls);
        assert_eq!(back.wakes, original.wakes);
        assert_eq!(back.tolerance_pct, original.tolerance_pct);
    }

    #[test]
    fn save_scope_mirrors_what_the_args_measured() {
        let mut common = CommonArgs {
            pkg: Some("mylib".into()),
            ..CommonArgs::default()
        };
        assert!(matches!(
            SaveScope::of(&common), SaveScope::BenchFull(Some(p)) if p == "mylib"
        ));
        common.filter = Some("group::".into());
        assert!(matches!(
            SaveScope::of(&common),
            SaveScope::BenchFiltered(_)
        ));
        common.filter = None;
        common.backend = Some("buildcost".into());
        assert!(matches!(
            SaveScope::of(&common), SaveScope::Buildcost(Some(p)) if p == "mylib"
        ));
    }

    const HEAD: &str = r#"
version = 4

[[package]]
name = "serde"
version = "1.0.200"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "soothfast"
version = "0.1.7"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "soothfast-measure",
]

[[package]]
name = "soothfast-measure"
version = "0.1.7"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

    #[test]
    fn pins_family_crates_to_heads_versions() {
        let base = HEAD.replace("0.1.7", "0.1.5");
        assert_eq!(
            harness_mismatches(HEAD, &base),
            vec![
                ("soothfast".into(), "0.1.5".into(), "0.1.7".into()),
                ("soothfast-measure".into(), "0.1.5".into(), "0.1.7".into()),
            ]
        );
    }

    #[test]
    fn matching_locks_and_foreign_crates_need_nothing() {
        assert!(harness_mismatches(HEAD, HEAD).is_empty());
        // serde is measured code, not the harness — its bumps must stay gated.
        let base = HEAD.replace("1.0.200", "1.0.100");
        assert!(harness_mismatches(HEAD, &base).is_empty());
    }

    #[test]
    fn path_dependencies_are_skipped() {
        // Gating soothfast itself: family crates are path deps, no source line.
        let no_source = HEAD.replace(
            "source = \"registry+https://github.com/rust-lang/crates.io-index\"",
            "",
        );
        let base = no_source.replace("0.1.7", "0.1.5");
        assert!(harness_mismatches(&no_source, &base).is_empty());
    }

    #[test]
    fn a_caret_incompatible_bump_is_still_a_mismatch() {
        let base = HEAD.replace("0.1.7", "0.2.0");
        assert_eq!(
            harness_mismatches(HEAD, &base),
            vec![
                ("soothfast".into(), "0.2.0".into(), "0.1.7".into()),
                ("soothfast-measure".into(), "0.2.0".into(), "0.1.7".into()),
            ]
        );
    }

    fn wanted() -> BTreeMap<&'static str, &'static str> {
        BTreeMap::from([("soothfast", "0.3.1")])
    }

    #[test]
    fn a_requirement_conflict_is_told_apart_from_an_unreachable_index() {
        assert!(super::is_requirement_conflict(
            "error: failed to select a version for the requirement `soothfast = \"^0.2.0\"`\n\
             candidate versions found which didn't match: 0.3.1\n"
        ));
        assert!(!super::is_requirement_conflict(
            "error: failed to get `soothfast` as a dependency\n\
             Caused by: network failure seems to have happened\n"
        ));
    }

    #[test]
    fn every_requirement_shape_moves_to_heads_version() {
        let manifest = r#"[package]
name = "demo"

[dependencies]
soothfast = "0.2.0"
serde = "1.0.200"

[dev-dependencies]
soothfast = { version = "0.2.0", features = ["runner"], default-features = false }

[target.'cfg(unix)'.dependencies.soothfast]
version = "0.2.0"
optional = true
"#;
        let got = super::retarget_requirements(manifest, &wanted()).expect("rewritten");
        assert_eq!(got.matches("\"0.3.1\"").count(), 3);
        assert!(!got.contains("0.2.0"));
        assert!(got.contains("features = [\"runner\"], default-features = false"));
        assert!(got.contains("serde = \"1.0.200\""));
        assert!(got.contains("optional = true"));
    }

    #[test]
    fn a_manifest_with_nothing_to_move_is_left_alone() {
        let manifest = "[dependencies]\nsoothfast = \"0.3.1\"\nserde = \"1\"\n";
        assert!(super::retarget_requirements(manifest, &wanted()).is_none());
        // A path dep resolves from the tree, not from a requirement.
        let path_dep =
            "[dependencies]\nsoothfast = { path = \"../soothfast\", version = \"0.2.0\" }\n";
        assert!(super::retarget_requirements(path_dep, &wanted()).is_none());
        // `soothfast` outside a dependency table names something else.
        let other = "[features]\nsoothfast = \"0.2.0\"\n";
        assert!(super::retarget_requirements(other, &wanted()).is_none());
    }

    #[test]
    fn rewriting_keeps_the_rest_of_the_line() {
        let manifest = "[dependencies]\nsoothfast = \"0.2.0\" # the harness\n";
        let got = super::retarget_requirements(manifest, &wanted()).expect("rewritten");
        assert_eq!(got, "[dependencies]\nsoothfast = \"0.3.1\" # the harness\n");
    }

    #[test]
    fn an_unpinned_crate_survives_as_the_runs_verdict_context() {
        let matched = super::sync_outcome(&[]);
        assert!(!matched.is_mismatched());
        assert!(matched.unpinned().is_empty());
        let missed = super::sync_outcome(&[("soothfast".into(), "0.2.0".into(), "0.3.1".into())]);
        assert!(missed.is_mismatched());
        assert_eq!(missed.unpinned(), ["soothfast 0.2.0 -> 0.3.1"]);
        assert_eq!(
            super::HarnessSync::from_unpinned(missed.unpinned().to_vec()),
            missed
        );
    }

    #[test]
    fn a_toolchain_that_refuses_the_tracking_flags_is_told_from_a_failed_build() {
        let refused = [
            "error: Unrecognized option: 'emit'",
            "error: invalid message format specifier: `json-render-diagnostics`",
            "error: unexpected argument '--emit' found",
        ];
        for stderr in refused {
            assert!(super::rejects_tracking_flags(stderr), "{stderr}");
        }
        let failed = "error[E0425]: cannot find value `x` in this scope\n\
                      Caused by: process didn't exit successfully: `rustdoc --emit=dep-info=x.d`";
        assert!(!super::rejects_tracking_flags(failed));
    }

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

    #[test]
    fn only_a_missing_toolchain_is_blamed_on_the_toolchain() {
        use super::ToolchainTrouble::{Components, Missing};
        let tc = "nightly-2026-08-07";
        let blamed = [
            (
                "error: toolchain 'nightly-2026-08-07-x86_64-unknown-linux-gnu' is not installed",
                Missing,
            ),
            (
                "error: 'nightly-2026-08-07' is not installed\nhelp: run `rustup toolchain install`",
                Missing,
            ),
            (
                "error: toolchain 'nightly-2026-08-07' is not installable",
                Missing,
            ),
            ("error: no release found for 'nightly-2026-08-07'", Missing),
            ("error: no such command: `+nightly-2026-08-07`", Missing),
            (
                "info: syncing channel updates for 'nightly-2026-08-07'\n\
                 error: could not download file from \
                 'https://static.rust-lang.org/dist/2026-08-07/channel-rust-nightly.toml.sha256'",
                Missing,
            ),
            (
                "error: failed to install toolchain 'nightly-2026-08-07'",
                Missing,
            ),
            (
                "error: component 'rust-docs-json' for target 'x86_64-unknown-linux-gnu' \
                 is unavailable for download for channel 'nightly-2026-08-07'",
                Components,
            ),
        ];
        for (stderr, trouble) in blamed {
            assert_eq!(
                super::toolchain_trouble(stderr, tc),
                Some(trouble),
                "{stderr}"
            );
        }
        let elsewhere = [
            "error: no library targets found in package `tool`",
            "error: 'rustdoc' is not installed for the toolchain 'nightly-2026-08-07'",
            "error: 'cargo-clippy' is not installed for the toolchain \
             'nightly-2026-08-07-x86_64-unknown-linux-gnu'",
            "error: component download failed for rust-docs-x86_64-unknown-linux-gnu",
            "error: toolchain 'stable' is not installed",
            "error: failed to run custom build command for `foo-sys v0.1.0`\n\
             --- stderr\n  error: error downloading libfoo for 'nightly-2026-08-07'",
            "error: foo-sys@0.1.0: failed to install libfoo for 'nightly-2026-08-07'",
        ];
        for stderr in elsewhere {
            assert_eq!(super::toolchain_trouble(stderr, tc), None, "{stderr}");
        }
    }

    #[test]
    fn a_failing_command_keeps_its_stderr_and_stdout() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "echo out; echo oops >&2; exit 3"]);
        let built = super::run_teeing_stderr(cmd).unwrap();
        assert_eq!(built.status.code(), Some(3));
        assert_eq!(built.stdout, b"out\n");
        assert_eq!(built.stderr, "oops\n");
    }

    #[test]
    fn a_plain_directory_in_the_repo_is_not_a_worktree_root() {
        let Ok(root) = super::workspace_root() else {
            return;
        };
        assert!(!super::is_worktree_root(&root.join("cargo-soothfast")));
    }

    #[test]
    fn the_workspace_root_is_a_worktree_root_only_in_a_linked_worktree() {
        let Ok(root) = super::workspace_root() else {
            return;
        };
        assert_eq!(super::is_worktree_root(&root), root.join(".git").is_file());
    }
}
