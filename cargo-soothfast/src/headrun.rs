//! HEAD's bench binary as the run cache sees it: its digest, what it resolves
//! on this host, and the runs stored for it. Shared by `gate` and `measure`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use crate::agreement::{self, Agreement, Tolerance};
use crate::buildstamp::{self, BuildStamp};
use crate::gate;
use crate::invoke::{self, CommonArgs, ItemMetrics, Run};
use crate::runcache::{self, Runs};

/// Build HEAD's bench binary once and name it: its digest, and the run cache
/// keyed on what it resolves here. `cmd` prefixes the line saying which.
pub fn probe<'a>(
    cmd: &str,
    common: &'a CommonArgs,
    stamp: &'a BuildStamp,
) -> (Option<String>, Runs<'a>) {
    let exe = invoke::bench_executable(common, None, None);
    let digest = exe.as_deref().and_then(binary_digest);
    let runs = Runs::new(stamp, common, resolved_env(cmd, common, exe.as_deref()));
    (digest, runs)
}

/// Measure HEAD's bench binary for `measure`. Under callgrind, a run already
/// stored for this binary supplies the counters and allocations, and only
/// timing is measured again; everything else measures in full and stores
/// the result for the next gate or measure.
pub fn measure(common: &CommonArgs, reuse: bool) -> Result<Run, String> {
    let stamp = buildstamp::capture(common.codegen_units_stamp().as_deref(), None);
    let (digest, runs) = probe("measure", common, &stamp);
    let stored = match (reuse_plan(reuse, runs.gating_backend()), digest.as_deref()) {
        (Plan::Reuse, Some(digest)) => {
            let label = format!("binary {digest}");
            match runs.load(digest, &label) {
                Some(doc) => Some((doc, label)),
                None => {
                    println!("measure: no stored run for binary {digest}; measuring in full");
                    None
                }
            }
        }
        (Plan::Full(why), _) => {
            println!("measure: {why}; measuring in full");
            None
        }
        (Plan::Reuse, None) => None,
    };
    if let Some((doc, label)) = stored {
        let merged = if has_callgrind_counts(&doc) {
            let timing = run_head(common, &["--skip-gating-counters"], stamp.clone())?;
            merge_stored(&doc, &label, timing)
        } else {
            None
        };
        match merged {
            Some(run) => {
                println!("measure: reusing counters for {label}; timing measured fresh");
                return Ok(run);
            }
            None => println!(
                "measure: stored run for {label} lacks callgrind counts for these items; measuring in full"
            ),
        }
    }
    let run = run_head(common, &[], stamp.clone())?;
    let settled = settle("measure", "this run", &run, || {
        run_head(common, RECOUNT, stamp.clone()).ok()
    });
    cache_head(&runs, &run, digest.as_deref(), &settled);
    Ok(run)
}

#[derive(Debug, PartialEq)]
enum Plan {
    Reuse,
    Full(&'static str),
}

/// Only callgrind is worth reusing: its counts take minutes and are exact.
/// perfcnt counts take seconds, and a sweep taken without them would run on
/// walltime instead, so its complexity verdicts could differ from a full run.
fn reuse_plan(reuse: bool, backend: Option<&str>) -> Plan {
    match backend {
        _ if !reuse => Plan::Full("--no-reuse"),
        None => Plan::Full("the run cache is off"),
        Some("callgrind") => Plan::Reuse,
        Some(_) => Plan::Full("counts other than callgrind are cheap to take"),
    }
}

fn run_head(common: &CommonArgs, extra: &[&str], stamp: BuildStamp) -> Result<Run, String> {
    let records = invoke::run_bench(common, extra).map_err(|e| e.to_string())?;
    let mut run = invoke::collect(&records);
    run.build = Some(stamp);
    Ok(run)
}

/// HEAD's run from a stored run's counters plus a fresh timing-only pass,
/// which also carries every assertion verdict. Every field but walltime,
/// fingerprint/covers, and tolerance_pct (all read off the binary the fresh
/// pass actually ran) comes from the stored item, so a counter added to
/// `ItemMetrics` later carries over without a change here. `None` unless the
/// stored run counted every item the timing pass measured.
fn merge_stored(stored: &Value, measured_from: &str, timing: Run) -> Option<Run> {
    let counted = invoke::run_from_items_value(&stored["items"]);
    if !has_callgrind_counts(stored) || !counted.items.keys().eq(timing.items.keys()) {
        return None;
    }
    let mut run = timing;
    let mut metrics = BTreeSet::new();
    for (id, item) in &mut run.items {
        let stored = &counted.items[id];
        let merged = ItemMetrics {
            fingerprint: item.fingerprint.clone(),
            covers: item.covers.clone(),
            tolerance_pct: item.tolerance_pct,
            median_ns: item.median_ns,
            mad_ns: item.mad_ns,
            p99_ns: item.p99_ns,
            wall_rounds: item.wall_rounds.clone(),
            ..stored.clone()
        };
        metrics.extend(
            COUNTER_METRICS
                .iter()
                .filter(|(_, present)| present(&merged))
                .map(|(name, _)| *name),
        );
        *item = merged;
    }
    run.gating_backend = Some("callgrind".into());
    run.reused = Some(invoke::Reused {
        from: measured_from.to_string(),
        metrics: metrics.into_iter().collect(),
    });
    Some(run)
}

/// A counter's backend-qualified name paired with its presence check.
type CounterCheck = (&'static str, fn(&ItemMetrics) -> bool);

/// Backend-qualified names for the counters `merge_stored` may carry over,
/// checked against the merged item so `reused.metrics` reports exactly what
/// came from the stored run instead of a fixed list.
const COUNTER_METRICS: &[CounterCheck] = &[
    ("perfcnt.instructions", |m| m.instructions.is_some()),
    ("perfcnt.cycles", |m| m.cycles.is_some()),
    ("perfcnt.cache_refs", |m| m.cache_refs.is_some()),
    ("callgrind.ir", |m| m.ir.is_some()),
    ("alloc.allocs", |m| m.allocs.is_some()),
    ("alloc.bytes", |m| m.bytes.is_some()),
    ("asyncexec.polls", |m| m.polls.is_some()),
    ("asyncexec.wakes", |m| m.wakes.is_some()),
    ("buildcost.build_ms", |m| m.build_ms.is_some()),
    ("buildcost.size_bytes", |m| m.size_bytes.is_some()),
];

/// Whether every item of a stored run carries Ir. Checked before the timing
/// pass, so a run that cannot be merged costs nothing to find out.
fn has_callgrind_counts(stored: &Value) -> bool {
    let items = invoke::run_from_items_value(&stored["items"]).items;
    !items.is_empty() && items.values().all(|m| m.ir.is_some())
}

/// HEAD's own measurement, if a run already cached one under this binary and
/// these conditions, e.g. the `gate` run `gate accept` follows moments later.
/// Avoids a second full measurement of the same code just to build the
/// accept report.
pub fn head_from_runcache(runs: &Runs, digest: Option<&str>) -> Option<(Run, String)> {
    let (id, measured_from) = head_cache_id(digest)?;
    let doc = runs.load(&id, &measured_from)?;
    Some((invoke::run_from_items_value(&doc["items"]), measured_from))
}

/// What HEAD's own run is stored under, and the label saying what it was
/// measured from. A dirty tree has no commit naming what was built, but the
/// bench binary integrates every input that decides the numbers, so it can
/// name its own measurement when the commit cannot.
fn head_cache_id(digest: Option<&str>) -> Option<(String, String)> {
    if invoke::tree_is_clean()
        && let Ok(sha) = invoke::git(&["rev-parse", "HEAD"])
    {
        let sha = sha.trim().to_string();
        return Some((sha.clone(), sha));
    }
    let digest = digest?;
    Some((digest.to_string(), format!("binary {digest}")))
}

/// Digest of a bench binary's loaded sections.
pub fn binary_digest(exe: &Path) -> Option<String> {
    loaded_section_bytes(exe).map(|b| format!("{:016x}", soothfast_registry::fnv1a(&b)))
}

/// What HEAD's bench binary resolves on this host, asked of the binary itself.
/// `None` disables the run cache for this invocation, and says so under `cmd`.
fn resolved_env(cmd: &str, common: &CommonArgs, exe: Option<&Path>) -> Option<invoke::HostEnv> {
    let resolved = exe
        .ok_or_else(|| "HEAD's bench binary did not build".to_string())
        .and_then(|exe| invoke::host_env(exe, common.backend.as_deref()));
    match resolved {
        Ok(env) => {
            println!(
                "{cmd}: run cache keyed on {} ({})",
                env.gating_backend,
                runcache::host(&env)
            );
            Some(env)
        }
        Err(why) => {
            println!(
                "{cmd}: run cache disabled for this invocation: {why} (harness {}, CLI {})",
                invoke::harness_versions(),
                env!("CARGO_PKG_VERSION")
            );
            None
        }
    }
}

/// Runner args that read only the perfcnt counters again.
pub const RECOUNT: &[&str] = &["--backend", "perfcnt"];

/// What a store may persist of one measured run.
#[derive(Debug, PartialEq)]
pub enum Settled {
    /// Not a perfcnt run: stored as measured.
    AsMeasured,
    /// perfcnt instruction counts that agreed across readings, per item.
    Counts(BTreeMap<String, u64>),
    /// Some item's readings never agreed: store nothing.
    Refused,
}

/// Settle a run before it is stored. A stored run is reused by every later
/// gate with its key, so perfcnt counts are read again with `recount`, and a
/// third time where two readings disagree; callgrind is exact and walltime
/// is never reused, so those runs store as measured. `what` names the run in
/// the line printed when nothing is stored.
pub fn settle(
    cmd: &str,
    what: &str,
    run: &Run,
    mut recount: impl FnMut() -> Option<Run>,
) -> Settled {
    if run.gating_backend.as_deref() != Some("perfcnt") {
        return Settled::AsMeasured;
    }
    let outcome = match recount() {
        Some(second) => settle_counts(run, &second, recount),
        None => Err(Refusal::Unread),
    };
    match outcome {
        Ok(counts) => return Settled::Counts(counts),
        Err(Refusal::Unread) => {
            println!("{cmd}: not storing {what}: its perfcnt counters could not be read again");
        }
        Err(Refusal::Spread(spread)) => {
            for (id, low, high) in spread {
                let pct = (high - low) as f64 / low.max(1) as f64 * 100.0;
                println!(
                    "{cmd}: not storing {what}: {id} instructions read {low} to {high} ({pct:.1}% apart)"
                );
            }
        }
    }
    Settled::Refused
}

#[derive(Debug, PartialEq)]
enum Refusal {
    /// A recount did not produce a reading for every item.
    Unread,
    /// Items whose readings never agreed, with their lowest and highest.
    Spread(Vec<(String, u64, u64)>),
}

fn settle_counts(
    first: &Run,
    second: &Run,
    third: impl FnOnce() -> Option<Run>,
) -> Result<BTreeMap<String, u64>, Refusal> {
    let tol = Tolerance {
        pct: gate::COUNTER_FLAT_PCT,
        abs: gate::COUNTER_FLAT_ABS,
    };
    let reading = |run: &Run, id: &str| run.items.get(id).and_then(|m| m.instructions);
    let mut pairs = Vec::new();
    for (id, m) in &first.items {
        if let Some(a) = m.instructions {
            pairs.push((id, a, reading(second, id).ok_or(Refusal::Unread)?));
        }
    }
    let disagree = pairs.iter().any(|&(_, a, b)| !agreement::within(a, b, tol));
    let third = if disagree {
        Some(third().ok_or(Refusal::Unread)?)
    } else {
        None
    };
    let mut counts = BTreeMap::new();
    let mut spread = Vec::new();
    for (id, a, b) in pairs {
        let c = match &third {
            Some(t) => Some(reading(t, id).ok_or(Refusal::Unread)?),
            None => None,
        };
        match agreement::settle(a, b, || c.unwrap_or(a), tol) {
            Agreement::Store(v) => {
                counts.insert(id.clone(), v);
            }
            Agreement::Refuse { low, high } => spread.push((id.clone(), low, high)),
        }
    }
    if spread.is_empty() {
        Ok(counts)
    } else {
        Err(Refusal::Spread(spread))
    }
}

/// `doc` as a store may keep it, or `None` when nothing may be stored.
pub fn settled_doc(mut doc: Value, settled: &Settled) -> Option<Value> {
    match settled {
        Settled::AsMeasured => Some(doc),
        Settled::Refused => None,
        Settled::Counts(counts) => {
            for (id, v) in counts {
                doc["items"][id]["perfcnt"]["instructions"] = serde_json::json!(v);
            }
            Some(doc)
        }
    }
}

/// Keep HEAD's run so the next gate finds its reference already measured.
/// Stored under the bench binary's digest always, and under HEAD's SHA as
/// well when the tree is clean enough for the commit to name what was built.
pub fn cache_head(runs: &Runs, head: &Run, digest: Option<&str>, settled: &Settled) {
    if let Some(doc) = settled_doc(ref_doc(head), settled) {
        cache_head_doc(runs, &doc, digest);
    }
}

/// Keep `doc` as HEAD's measurement.
pub fn cache_head_doc(runs: &Runs, doc: &Value, digest: Option<&str>) {
    if let Some(digest) = digest {
        runs.store(digest, doc);
    }
    if !invoke::tree_is_clean() {
        return;
    }
    let Ok(sha) = invoke::git(&["rev-parse", "HEAD"]) else {
        return;
    };
    runs.store(sha.trim(), doc);
}

/// A measured run in the reference-document shape `compare` reads.
pub fn ref_doc(run: &Run) -> Value {
    let mut doc = serde_json::json!({ "version": 1 });
    if let Some(n) = run.noise_pct {
        doc["noise_pct"] = serde_json::json!(n);
    }
    if let Some(b) = &run.build {
        doc["build"] = b.to_json();
    }
    doc["items"] = invoke::run_to_items_value(run);
    doc
}

/// Every allocatable section carrying file contents, name-tagged and
/// concatenated. Whole-file comparison would never match: debug info embeds
/// the absolute build path, and the two sides build in different directories.
pub fn loaded_section_bytes(exe: &Path) -> Option<Vec<u8>> {
    let out = std::process::Command::new("objdump")
        .arg("-h")
        .arg(exe)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let image = std::fs::read(exe).ok()?;
    let listing = String::from_utf8_lossy(&out.stdout);
    let mut lines = listing.lines();
    let mut buf = Vec::new();
    while let Some(header) = lines.next() {
        let Some((name, size, offset)) = section_header(header) else {
            continue;
        };
        let flags = lines.next()?;
        if !digestible(name, flags) {
            continue;
        }
        let bytes = image.get(offset..offset.checked_add(size)?)?;
        buf.extend_from_slice(name.as_bytes());
        buf.push(0);
        buf.extend_from_slice(bytes);
    }
    (!buf.is_empty()).then_some(buf)
}

/// Sections that carry the binary's identity: loaded into the image and
/// backed by file contents. `.note.gnu.build-id` is a hash of the whole
/// output, debug info included, so it differs between builds of identical
/// code.
fn digestible(name: &str, flags: &str) -> bool {
    flags.contains("ALLOC") && flags.contains("CONTENTS") && name != ".note.gnu.build-id"
}

/// `Idx Name Size VMA LMA File-off Algn` from one `objdump -h` header line.
fn section_header(line: &str) -> Option<(&str, usize, usize)> {
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() < 7 || f[0].parse::<u32>().is_err() {
        return None;
    }
    let size = usize::from_str_radix(f[2], 16).ok()?;
    let offset = usize::from_str_radix(f[5], 16).ok()?;
    Some((f[1], size, offset))
}

#[cfg(test)]
mod tests {
    use super::{
        Plan, Settled, cache_head, digestible, loaded_section_bytes, merge_stored, ref_doc,
        reuse_plan, settle, settled_doc,
    };
    use crate::buildstamp::BuildStamp;
    use crate::invoke::{AssertionOutcome, CommonArgs, HostEnv, ItemMetrics, Run};
    use crate::runcache::Runs;
    use serde_json::json;
    use std::cell::Cell;

    fn perfcnt(instructions: u64) -> Run {
        let mut run = Run {
            gating_backend: Some("perfcnt".into()),
            ..Run::default()
        };
        run.items.insert(
            "pkg::a".into(),
            ItemMetrics {
                instructions: Some(instructions),
                ..ItemMetrics::default()
            },
        );
        run
    }

    #[test]
    fn only_perfcnt_runs_are_read_again() {
        let mut run = perfcnt(3_504_110);
        run.gating_backend = Some("callgrind".into());
        let got = settle("gate", "HEAD's run", &run, || panic!("callgrind is exact"));
        assert_eq!(got, Settled::AsMeasured);
    }

    #[test]
    fn agreeing_perfcnt_readings_store_the_first() {
        let got = settle("gate", "HEAD's run", &perfcnt(3_504_108), || {
            Some(perfcnt(3_504_111))
        });
        assert_eq!(got, Settled::Counts([("pkg::a".into(), 3_504_108)].into()));
    }

    #[test]
    fn a_bad_perfcnt_reading_is_outvoted_by_a_third() {
        let calls = Cell::new(0);
        let got = settle("gate", "the merge-base run", &perfcnt(3_271_181), || {
            calls.set(calls.get() + 1);
            Some(perfcnt(if calls.get() == 1 {
                3_504_110
            } else {
                3_504_111
            }))
        });
        assert_eq!(calls.get(), 2);
        assert_eq!(got, Settled::Counts([("pkg::a".into(), 3_504_110)].into()));
    }

    #[test]
    fn disagreeing_perfcnt_readings_store_nothing() {
        let calls = Cell::new(0);
        let got = settle("gate", "the merge-base run", &perfcnt(3_271_181), || {
            calls.set(calls.get() + 1);
            Some(perfcnt(if calls.get() == 1 {
                3_504_110
            } else {
                3_389_000
            }))
        });
        assert_eq!(got, Settled::Refused);
        assert_eq!(settle("gate", "x", &perfcnt(1), || None), Settled::Refused);
    }

    #[test]
    fn a_settled_count_replaces_the_stored_reading() {
        let doc = json!({ "items": { "pkg::a": { "perfcnt": { "instructions": 3_271_181 } } } });
        let counts = Settled::Counts([("pkg::a".into(), 3_504_110)].into());
        let stored = settled_doc(doc.clone(), &counts).expect("stored");
        assert_eq!(
            stored["items"]["pkg::a"]["perfcnt"]["instructions"],
            3_504_110
        );
        assert_eq!(
            settled_doc(doc.clone(), &Settled::AsMeasured),
            Some(doc.clone())
        );
        assert_eq!(settled_doc(doc, &Settled::Refused), None);
    }

    #[test]
    fn a_refused_perfcnt_run_is_not_stored() {
        let stamp = BuildStamp {
            rustc: "1.88.0".into(),
            codegen_units: "1".into(),
            profiles: "p".into(),
            rustflags: "f".into(),
        };
        let common = CommonArgs {
            pkg: Some("test-headrun-refused".into()),
            ..CommonArgs::default()
        };
        let env = HostEnv {
            gating_backend: "perfcnt".into(),
            guest: None,
        };
        let runs = Runs::new(&stamp, &common, Some(env));
        cache_head(
            &runs,
            &perfcnt(3_271_181),
            Some("test-headrun-refused"),
            &Settled::Refused,
        );
        assert!(runs.load("test-headrun-refused", "x").is_none());
    }

    fn stored(items: serde_json::Value) -> serde_json::Value {
        json!({ "version": 1, "items": items })
    }

    fn timing() -> Run {
        let mut run = Run {
            gating_backend: Some("walltime".into()),
            ..Run::default()
        };
        run.items.insert(
            "pkg::a".into(),
            ItemMetrics {
                fingerprint: "fp".into(),
                median_ns: Some(120.0),
                p99_ns: Some(150.0),
                allocs: Some(9),
                ..ItemMetrics::default()
            },
        );
        run.assertions.push(AssertionOutcome {
            id: "pkg::a".into(),
            kind: "p99".into(),
            ok: true,
            detail: "p99 150ns <= 1000ns".into(),
        });
        run
    }

    #[test]
    fn only_callgrind_reuses_a_stored_run() {
        assert_eq!(reuse_plan(true, Some("callgrind")), Plan::Reuse);
        assert!(matches!(reuse_plan(true, Some("perfcnt")), Plan::Full(_)));
        assert!(matches!(reuse_plan(true, Some("walltime")), Plan::Full(_)));
        assert!(matches!(reuse_plan(true, None), Plan::Full(_)));
    }

    #[test]
    fn no_reuse_measures_in_full() {
        assert_eq!(
            reuse_plan(false, Some("callgrind")),
            Plan::Full("--no-reuse")
        );
    }

    #[test]
    fn a_hit_takes_counts_from_the_stored_run_and_timing_from_the_fresh_pass() {
        let doc = stored(json!({ "pkg::a": {
            "fingerprint": "fp",
            "callgrind": { "ir": 4567 },
            "alloc": { "allocs": 3, "bytes": 96 },
            "walltime": { "median_ns": 999.0, "p99_ns": 1999.0 },
        }}));
        let run = merge_stored(&doc, "binary 0123456789abcdef", timing()).expect("hit");
        let item = &run.items["pkg::a"];
        assert_eq!(item.ir, Some(4567));
        assert_eq!((item.allocs, item.bytes), (Some(3), Some(96)));
        assert_eq!((item.median_ns, item.p99_ns), (Some(120.0), Some(150.0)));
        assert_eq!(run.assertions.len(), 1);
        assert_eq!(run.gating_backend.as_deref(), Some("callgrind"));
        assert_eq!(run.reused.expect("reused").from, "binary 0123456789abcdef");
    }

    #[test]
    fn a_stored_run_without_callgrind_counts_is_a_miss() {
        let perfcnt = stored(json!({ "pkg::a": {
            "fingerprint": "fp",
            "perfcnt": { "instructions": 4567 },
            "alloc": { "allocs": 3, "bytes": 96 },
        }}));
        assert!(merge_stored(&perfcnt, "binary x", timing()).is_none());
        let timing_only = stored(json!({ "pkg::a": {
            "fingerprint": "fp",
            "walltime": { "median_ns": 999.0 },
        }}));
        assert!(merge_stored(&timing_only, "binary x", timing()).is_none());
    }

    #[test]
    fn a_stored_run_of_other_items_is_a_miss() {
        let doc = stored(json!({ "pkg::b": { "fingerprint": "fp", "callgrind": { "ir": 1 } } }));
        assert!(merge_stored(&doc, "binary x", timing()).is_none());
    }

    #[test]
    fn a_hit_carries_every_counter_the_stored_run_has() {
        let mut full = Run {
            gating_backend: Some("callgrind".into()),
            ..Run::default()
        };
        full.items.insert(
            "pkg::a".into(),
            ItemMetrics {
                fingerprint: "stored-fp".into(),
                covers: "stored-covers".into(),
                median_ns: Some(999.0),
                mad_ns: Some(9.0),
                p99_ns: Some(1999.0),
                wall_rounds: vec![990.0, 1010.0],
                instructions: Some(42),
                cycles: Some(100),
                cache_refs: Some(7),
                ir: Some(4567),
                allocs: Some(3),
                bytes: Some(96),
                polls: Some(5),
                wakes: Some(2),
                ..ItemMetrics::default()
            },
        );
        let doc = ref_doc(&full);

        let mut fresh = timing();
        fresh.items.get_mut("pkg::a").unwrap().allocs = None;

        let run = merge_stored(&doc, "binary abc", fresh).expect("hit");
        let item = &run.items["pkg::a"];
        assert_eq!(item.instructions, Some(42));
        assert_eq!(item.cycles, Some(100));
        assert_eq!(item.cache_refs, Some(7));
        assert_eq!(item.ir, Some(4567));
        assert_eq!(item.allocs, Some(3));
        assert_eq!(item.bytes, Some(96));
        assert_eq!(item.polls, Some(5));
        assert_eq!(item.wakes, Some(2));
        assert_eq!((item.median_ns, item.p99_ns), (Some(120.0), Some(150.0)));

        let mut metrics = run.reused.expect("reused").metrics;
        metrics.sort_unstable();
        assert_eq!(
            metrics,
            vec![
                "alloc.allocs",
                "alloc.bytes",
                "asyncexec.polls",
                "asyncexec.wakes",
                "callgrind.ir",
                "perfcnt.cache_refs",
                "perfcnt.cycles",
                "perfcnt.instructions",
            ]
        );
    }

    #[test]
    fn section_extraction_returns_none_for_a_non_elf_file() {
        let path = std::env::temp_dir().join(format!("soothfast-not-elf-{}", std::process::id()));
        std::fs::write(&path, b"just text, no sections").unwrap();
        assert_eq!(loaded_section_bytes(&path), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn section_extraction_returns_none_for_a_missing_file() {
        assert_eq!(
            loaded_section_bytes(std::path::Path::new("/nonexistent/soothfast-bench")),
            None
        );
    }

    #[test]
    fn an_executable_matches_itself() {
        if !objdump_present() {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let a = loaded_section_bytes(&exe).expect("test binary has loadable sections");
        assert_eq!(Some(a), loaded_section_bytes(&exe));
    }

    #[test]
    fn the_digest_covers_more_than_the_text_section() {
        if !objdump_present() {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let all = loaded_section_bytes(&exe).unwrap();
        assert!(all.windows(8).any(|w| w == b".rodata\0"));
        assert!(all.windows(6).any(|w| w == b".text\0"));
    }

    #[test]
    fn the_build_id_note_is_not_digestible() {
        const LOADED: &str = "CONTENTS, ALLOC, LOAD, READONLY, DATA";
        assert!(!digestible(".note.gnu.build-id", LOADED));
        assert!(digestible(".text", "CONTENTS, ALLOC, LOAD, READONLY, CODE"));
        assert!(digestible("linkme_MEASURED", LOADED));
        assert!(!digestible(".bss", "ALLOC"));
        assert!(!digestible(".debug_info", "CONTENTS, READONLY, DEBUGGING"));
    }

    fn objdump_present() -> bool {
        std::process::Command::new("objdump")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }
}
