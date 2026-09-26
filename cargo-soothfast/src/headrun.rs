//! HEAD's bench binary as the run cache sees it: its digest, what it resolves
//! on this host, and the runs stored for it. Shared by `gate` and `measure`.

use std::path::Path;

use serde_json::Value;

use crate::buildstamp::{self, BuildStamp};
use crate::invoke::{self, CommonArgs, Run};
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
    cache_head(&runs, &run, digest.as_deref());
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

/// HEAD's run from a stored run's callgrind counts and allocations plus a
/// fresh timing-only pass, which also carries every assertion verdict. `None`
/// unless the stored run counted every item the timing pass measured.
fn merge_stored(stored: &Value, measured_from: &str, timing: Run) -> Option<Run> {
    let counted = invoke::run_from_items_value(&stored["items"]);
    if !has_callgrind_counts(stored) || !counted.items.keys().eq(timing.items.keys()) {
        return None;
    }
    let mut run = timing;
    for (id, item) in &mut run.items {
        let stored = &counted.items[id];
        item.ir = stored.ir;
        item.allocs = stored.allocs;
        item.bytes = stored.bytes;
    }
    run.gating_backend = Some("callgrind".into());
    run.reused = Some(invoke::Reused {
        from: measured_from.to_string(),
        metrics: vec!["callgrind.ir", "alloc.allocs", "alloc.bytes"],
    });
    Some(run)
}

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

/// Keep HEAD's run so the next gate finds its reference already measured.
/// Stored under the bench binary's digest always, and under HEAD's SHA as
/// well when the tree is clean enough for the commit to name what was built.
pub fn cache_head(runs: &Runs, head: &Run, digest: Option<&str>) {
    cache_head_doc(runs, &ref_doc(head), digest);
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
    use super::{Plan, digestible, loaded_section_bytes, merge_stored, reuse_plan};
    use crate::invoke::{AssertionOutcome, ItemMetrics, Run};
    use serde_json::json;

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
