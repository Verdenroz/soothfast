//! Subprocess plumbing: run the user's soothfast bench binary via `cargo bench`,
//! parse its JSONL, and read/write baseline files.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use serde_json::{Value, json};

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
}

impl CommonArgs {
    /// Consume one flag if it belongs to the common set.
    pub fn try_parse<'a>(&mut self, arg: &str, it: &mut impl Iterator<Item = &'a String>) -> bool {
        let slot = match arg {
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
            _ => return false,
        };
        *slot = it.next().cloned();
        true
    }
}

fn bench_command(common: &CommonArgs, extra: &[&str], dir: Option<&Path>) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.arg("bench");
    if let Some(p) = &common.pkg {
        cmd.args(["-p", p]);
    }
    // Cargo-level flag: must precede the `--` separating runner args.
    if let Some(f) = &common.features {
        cmd.args(["--features", f]);
    }
    let target = common.target.as_deref().unwrap_or("soothfast");
    cmd.args(["--bench", target, "--"]);
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
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    cmd
}

/// Run the bench binary with `--json` (optionally in another worktree) and
/// parse JSONL records.
pub fn run_bench_in(
    common: &CommonArgs,
    extra: &[&str],
    dir: Option<&Path>,
) -> io::Result<Vec<Value>> {
    let mut args = vec!["--json"];
    args.extend_from_slice(extra);
    let target = common.target.as_deref().unwrap_or("soothfast");
    let mut cmd = bench_command(common, &args, dir);
    cmd.stdout(Stdio::piped());
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "cargo bench failed with {} (is the [[bench]] name = \"{target}\", harness = false target set up?)",
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
    let mut cmd = bench_command(common, extra, None);
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

/// Path of the compiled bench executable, via `cargo bench --no-run`'s JSON
/// messages. `None` on any failure: callers fall through to a normal
/// measured run, which surfaces the real error.
pub fn bench_executable(common: &CommonArgs, dir: Option<&Path>) -> Option<PathBuf> {
    let target = common.target.as_deref().unwrap_or("soothfast");
    let mut cmd = Command::new("cargo");
    cmd.args(["bench", "--no-run", "--message-format=json"]);
    if let Some(p) = &common.pkg {
        cmd.args(["-p", p]);
    }
    if let Some(f) = &common.features {
        cmd.args(["--features", f]);
    }
    cmd.args(["--bench", target]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
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
    pub allocs: Option<u64>,
    pub bytes: Option<u64>,
    pub polls: Option<u64>,
    pub wakes: Option<u64>,
    /// buildcost pseudo-items
    pub build_ms: Option<u64>,
    pub size_bytes: Option<u64>,
}

/// One assertion verdict from the runner.
#[derive(Debug, Clone)]
pub struct AssertionOutcome {
    pub id: String,
    pub kind: String,
    pub ok: bool,
    pub detail: String,
}

/// A full measurement run, keyed by item ID.
#[derive(Default, Debug)]
pub struct Run {
    pub noise_pct: Option<f64>,
    pub gating_backend: Option<String>,
    pub items: BTreeMap<String, ItemMetrics>,
    pub assertions: Vec<AssertionOutcome>,
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
                    Some("callgrind") => item.ir = m["ir"].as_u64(),
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
        if let Some(v) = m.median_ns {
            entry["walltime"] = json!({
                "median_ns": v,
                "mad_ns": m.mad_ns.unwrap_or(0.0),
                "p99_ns": m.p99_ns.unwrap_or(0.0),
            });
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
        items.insert(id.clone(), entry);
    }
    Value::Object(items)
}

/// Workspace root (parent of the workspace Cargo.toml), via cargo itself.
pub fn workspace_root() -> io::Result<PathBuf> {
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

fn baseline_path(name: &str) -> io::Result<PathBuf> {
    Ok(workspace_root()?
        .join(".soothfast")
        .join("baselines")
        .join(format!("{name}.json")))
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
    BenchFiltered,
    /// Buildcost run of one package (`None`: replace every buildcost entry).
    Buildcost(Option<String>),
}

/// First segment of a measured-item id (the package, `-` normalized to `_`).
fn id_pkg(id: &str) -> &str {
    id.split("::").next().unwrap_or(id)
}

/// Persist a run as a named baseline.
pub fn save_baseline(name: &str, run: &Run, scope: SaveScope) -> io::Result<PathBuf> {
    let new_items = run_to_items_value(run);
    let mut doc = load_baseline(name)?.unwrap_or_else(|| json!({ "version": 1, "items": {} }));
    let unix_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    doc["created_unix"] = json!(unix_now);
    if let Some(n) = run.noise_pct {
        doc["noise_pct"] = json!(n);
    }
    if let (Some(map), Some(new_map)) = (doc["items"].as_object_mut(), new_items.as_object()) {
        // Full runs replace their scope: renamed/removed items must not
        // haunt the baseline (the gate fails on GONE entries).
        match scope {
            SaveScope::BenchFull(Some(pkg)) => {
                let pkg = pkg.replace('-', "_");
                map.retain(|k, _| k.starts_with("buildcost::") || id_pkg(k) != pkg);
            }
            SaveScope::BenchFull(None) => map.retain(|k, _| k.starts_with("buildcost::")),
            SaveScope::Buildcost(Some(pkg)) => map.retain(|k, _| {
                k.strip_prefix("buildcost::")
                    .is_none_or(|rest| rest.split("::").next() != Some(pkg.as_str()))
            }),
            SaveScope::Buildcost(None) => map.retain(|k, _| !k.starts_with("buildcost::")),
            SaveScope::BenchFiltered => {}
        }
        for (k, v) in new_map {
            map.insert(k.clone(), v.clone());
        }
    }
    // Assertion verdicts ride along so renderers can show *verified* claims
    // ("proven O(n log n), zero-alloc") from produced data. Same scope rules
    // as items: a full run replaces its slice, keyed by the assertion's id.
    let replaced: Vec<String> = run.items.keys().cloned().collect();
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
        assertions.push(json!({
            "id": a.id, "kind": a.kind, "ok": a.ok, "detail": a.detail,
        }));
    }
    doc["assertions"] = Value::Array(assertions);

    let path = baseline_path(name)?;
    std::fs::create_dir_all(path.parent().expect("baseline path has a parent"))?;
    std::fs::write(&path, serde_json::to_string_pretty(&doc)?)?;
    Ok(path)
}

/// Load a named baseline, if present.
pub fn load_baseline(name: &str) -> io::Result<Option<Value>> {
    let path = baseline_path(name)?;
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
const RUSTDOC_FORMAT_VERSION: u64 = 61;

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

/// Latest modification time of `path` or, recursively, anything under it.
/// `target`/`.git` are skipped as build/VCS noise, not doc-relevant inputs.
fn newest_mtime(path: &Path) -> io::Result<Option<SystemTime>> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(None);
    };
    if meta.is_file() {
        return Ok(Some(meta.modified()?));
    }
    let mut newest = meta.modified().ok();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        if let Some(t) = newest_mtime(&entry.path())?
            && newest.is_none_or(|n| t > n)
        {
            newest = Some(t);
        }
    }
    Ok(newest)
}

/// Newest mtime among `pkg`'s own Cargo.toml/src/build.rs, the same files
/// for its transitive workspace-local dependencies, and the workspace
/// lockfile — the set of inputs `rustdoc_json_in`'s freshness check covers.
fn newest_relevant_mtime(pkg: &str, dir: Option<&Path>) -> io::Result<Option<SystemTime>> {
    let mut newest: Option<SystemTime> = None;
    let mut bump = |t: Option<SystemTime>| {
        if let Some(t) = t
            && newest.is_none_or(|n| t > n)
        {
            newest = Some(t);
        }
    };
    for crate_dir in local_crate_dirs(pkg, dir)? {
        bump(newest_mtime(&crate_dir.join("Cargo.toml"))?);
        bump(newest_mtime(&crate_dir.join("src"))?);
        bump(newest_mtime(&crate_dir.join("build.rs"))?);
    }
    let root = match dir {
        Some(d) => d.to_path_buf(),
        None => workspace_root()?,
    };
    bump(newest_mtime(&root.join("Cargo.lock"))?);
    Ok(newest)
}

/// Sidecar recording which (toolchain, features, visibility) produced
/// `json_path`'s current contents — its filename is keyed only by package
/// name, so a later call with different flags must not reuse it just
/// because the file's mtime looks fresh.
fn cache_meta_path(json_path: &Path) -> PathBuf {
    json_path.with_extension("json.soothfast-cache")
}

/// A prior extraction at `json_path`, if it was built with the same flags
/// requested now and is no older than the source it covers. Every failure
/// mode (missing sidecar, unreadable JSON, unresolvable source tree) falls
/// through to `None` — the caller then regenerates — because staleness must
/// never be silently assumed away.
fn read_fresh_cache(
    json_path: &Path,
    pkg: &str,
    dir: Option<&Path>,
    features: Option<&str>,
    visibility: Visibility,
    toolchain: &str,
) -> Option<Value> {
    let meta_text = std::fs::read_to_string(cache_meta_path(json_path)).ok()?;
    let meta: Value = serde_json::from_str(&meta_text).ok()?;
    let matches = meta["toolchain"].as_str() == Some(toolchain)
        && meta["features"].as_str() == features
        && meta["private"].as_bool() == Some(visibility == Visibility::Private);
    if !matches {
        return None;
    }
    let json_mtime = std::fs::metadata(json_path).ok()?.modified().ok()?;
    let newest_src = newest_relevant_mtime(pkg, dir).ok()??;
    if newest_src > json_mtime {
        return None;
    }
    let text = std::fs::read_to_string(json_path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Records the flags that produced `json_path`'s current contents, so a
/// later call can tell whether reusing it would silently drop feature-gated
/// or private items the caller now needs.
fn write_cache_meta(
    json_path: &Path,
    features: Option<&str>,
    visibility: Visibility,
    toolchain: &str,
) -> io::Result<()> {
    let meta = json!({
        "toolchain": toolchain,
        "features": features,
        "private": visibility == Visibility::Private,
    });
    std::fs::write(cache_meta_path(json_path), serde_json::to_string(&meta)?)
}

/// Build rustdoc JSON for a package (nightly-only job) and load it.
/// `dir` runs the build in another worktree; JSON lands in its target/doc.
///
/// Reuses a prior extraction instead of re-running `cargo rustdoc` when it's
/// still fresh: rustdoc's JSON backend re-serializes unconditionally on every
/// invocation even when the underlying compilation is fully cached, so
/// repeat calls for the same (package, features, visibility) — e.g. `docs
/// reference`, `report render`, and `coverage docs` all documenting the same
/// crate within one `make docs` run — otherwise pay that cost redundantly.
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

    if let Some(doc) = read_fresh_cache(&json_path, pkg, dir, features, visibility, &toolchain) {
        return Ok((doc, root));
    }

    let mut cmd = Command::new("cargo");
    cmd.args([&format!("+{toolchain}"), "rustdoc", "-p", pkg, "--lib"]);
    // Feature-gated items are invisible to the surface unless enabled.
    if let Some(f) = features {
        cmd.args(["--features", f]);
    }
    cmd.args(["--", "--output-format", "json", "-Zunstable-options"]);
    if visibility == Visibility::Private {
        cmd.arg("--document-private-items");
    }
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let status = cmd.status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "cargo +{toolchain} rustdoc failed with {status} \
             (is the {toolchain} toolchain installed?)"
        )));
    }
    let text = std::fs::read_to_string(&json_path)
        .map_err(|e| io::Error::other(format!("cannot read {}: {e}", json_path.display())))?;
    let doc: Value = serde_json::from_str(&text)?;
    warn_on_format_version(&doc);
    // Best-effort: a failed cache write only costs a future redundant
    // rebuild, not correctness, so it must not fail the extraction itself.
    let _ = write_cache_meta(&json_path, features, visibility, &toolchain);
    Ok((doc, root))
}

/// Check out the merge-base of HEAD and `refname` in a temp worktree, run
/// `f` against it, then remove the worktree (best effort).
pub fn with_merge_base_worktree<T>(
    refname: &str,
    f: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    let sha = git(&["merge-base", "HEAD", refname]).map_err(|e| e.to_string())?;
    let root = workspace_root().map_err(|e| e.to_string())?;
    let wt = root.join(".soothfast").join("worktrees").join(&sha);
    let wt_str = wt
        .to_str()
        .ok_or_else(|| "worktree path is not UTF-8".to_string())?;
    if !wt.exists() {
        git(&["worktree", "add", "--detach", wt_str, &sha]).map_err(|e| e.to_string())?;
    }
    let result = f(&wt);
    let _ = git(&["worktree", "remove", "--force", wt_str]);
    result
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

/// Pin the merge-base worktree's soothfast crates to HEAD's locked versions
/// so the reference bench binary embeds the same measurement harness. Best
/// effort: a pin that cargo rejects (offline, incompatible requirement)
/// warns and leaves the reference side as its own lock resolved it.
pub fn sync_harness_versions(wt: &Path) -> Result<(), String> {
    let head_path = workspace_root()
        .map_err(|e| e.to_string())?
        .join("Cargo.lock");
    let Ok(head) = std::fs::read_to_string(&head_path) else {
        return Ok(());
    };
    // One crate per pass, re-reading the lock: pinning the facade drags its
    // family along, and already-moved entries must not be warned about.
    for _ in 0..16 {
        let Ok(base) = std::fs::read_to_string(wt.join("Cargo.lock")) else {
            return Ok(());
        };
        let Some((name, from, to)) = harness_mismatches(&head, &base).into_iter().next() else {
            return Ok(());
        };
        println!(
            "gate: pinning {name} {from} -> {to} in the merge-base worktree (harness must match HEAD)"
        );
        let out = Command::new("cargo")
            .args(["update", "-p", &format!("{name}@{from}"), "--precise", &to])
            .current_dir(wt)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            eprintln!(
                "WARN: could not pin {name} ({}); the reference keeps its own harness — deltas may reflect the harness change itself",
                stderr.trim().lines().last().unwrap_or("no error output")
            );
            return Ok(());
        }
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
    use super::harness_mismatches;

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
}
