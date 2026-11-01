use super::*;
use crate::invoke::{AssertionOutcome, ItemMetrics};
use serde_json::{Value, json};

fn scratch(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("soothfast-save-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn core() -> CommonArgs {
    CommonArgs {
        pkg: Some("core".into()),
        ..CommonArgs::default()
    }
}

fn item(reused: Option<Reused>) -> ItemMetrics {
    ItemMetrics {
        median_ns: Some(10.0),
        mad_ns: Some(1.0),
        p99_ns: Some(12.0),
        ir: Some(900),
        allocs: Some(0),
        bytes: Some(0),
        reused,
        ..ItemMetrics::default()
    }
}

fn verdict(id: &str, kind: &str) -> AssertionOutcome {
    AssertionOutcome {
        id: id.into(),
        kind: kind.into(),
        ok: true,
        detail: String::new(),
    }
}

fn saved(name: &str, run: &Run, source: &HeadRun, failures: u32) -> Value {
    let root = scratch(name);
    let listed = ["core::a".to_string(), "core::b".to_string()];
    save_head(&root, "head", run, source, &core(), Some(&listed), failures).unwrap();
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    let _ = std::fs::remove_dir_all(&root);
    doc
}

fn run_of(items: Vec<(&str, ItemMetrics)>) -> Run {
    let mut run = Run::default();
    for (id, m) in items {
        run.items.insert(id.into(), m);
    }
    run
}

#[test]
fn a_failing_gate_saves_no_numbers_and_names_its_items() {
    let run = run_of(vec![("core::a", item(None))]);
    let doc = saved("failed", &run, &HeadRun::Measured, 1);
    assert_eq!(doc["items"], json!({}));
    let entries = unmeasured::read(&doc).unwrap();
    assert_eq!(entries[0].reason, Reason::GateFailed);
    assert_eq!(entries[0].items, ["core::a", "core::b"]);
}

#[test]
fn a_run_from_the_run_cache_keeps_its_counters_and_drops_its_walltime() {
    let run = run_of(vec![("core::a", item(None))]);
    let doc = saved("stored", &run, &HeadRun::Stored("abc".into()), 0);
    let a = &doc["items"]["core::a"];
    assert_eq!(a["callgrind"]["ir"], 900);
    assert!(a.get("walltime").is_none());
    assert_eq!(a["reused"]["from"], "abc");
    let entries = unmeasured::read(&doc).unwrap();
    assert_eq!(entries[0].reason, Reason::ReusedWalltime);
    assert_eq!(entries[0].metrics, ["walltime"]);
}

#[test]
fn an_item_spliced_from_the_item_cache_drops_only_its_walltime_and_verdicts() {
    let spliced = Reused {
        from: "item cache".into(),
        metrics: vec!["callgrind.ir", WALLTIME],
    };
    let mut run = run_of(vec![
        ("core::a", item(Some(spliced))),
        ("core::b", item(None)),
    ]);
    run.assertions = vec![verdict("core::a", "p99"), verdict("core::b", "complexity")];
    let doc = saved("spliced", &run, &HeadRun::Measured, 0);
    assert!(doc["items"]["core::a"].get("walltime").is_none());
    assert_eq!(
        doc["items"]["core::a"]["reused"]["metrics"],
        json!(["callgrind.ir"])
    );
    assert_eq!(doc["items"]["core::b"]["walltime"]["median_ns"], 10.0);
    let kept: Vec<&str> = doc["assertions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["id"].as_str())
        .collect();
    assert_eq!(kept, ["core::b"]);
}

#[test]
fn an_uncounted_pass_saves_its_own_walltime_and_lists_the_counters() {
    let mut m = item(None);
    m.ir = None;
    let mut run = run_of(vec![("core::a", m)]);
    run.assertions = vec![
        verdict("core::a", "complexity"),
        verdict("core::a", "alloc"),
    ];
    let doc = saved("uncounted", &run, &HeadRun::Uncounted, 0);
    assert_eq!(doc["items"]["core::a"]["walltime"]["median_ns"], 10.0);
    let entries = unmeasured::read(&doc).unwrap();
    assert_eq!(entries[0].reason, Reason::IdenticalBinaries);
    assert_eq!(entries[0].metrics, ["perfcnt", "callgrind"]);
    assert_eq!(doc["assertions"][0]["kind"], "alloc");
    assert_eq!(doc["assertions"].as_array().unwrap().len(), 1);
}

#[test]
fn an_errored_gate_names_the_listed_items_as_failed() {
    let root = scratch("errored");
    let listed = ["core::a".to_string(), "other::x".to_string()];
    save_errored(&root, "head", &core(), Some(&listed), "bench failed").unwrap();
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    let entries = unmeasured::read(&doc).unwrap();
    assert_eq!(entries[0].items, ["core::a", "other::x"]);
    assert!(entries[0].detail.contains("bench failed"));
    let _ = std::fs::remove_dir_all(&root);
}

/// A baseline under a fresh `name` root holding stale numbers of each
/// package's ids, with the package recorded as their owner.
fn stale(name: &str, owners: &[(&str, &[&str])]) -> std::path::PathBuf {
    let root = scratch(name);
    let mut doc = json!({ "version": 1, "items": {}, "owners": {} });
    for (package, ids) in owners {
        for id in *ids {
            doc["items"][*id] = json!({ "alloc": { "allocs": 1, "bytes": 8 } });
        }
        doc["owners"][*package] = json!(ids);
    }
    invoke::write_baseline_in(&root, "head", &doc).unwrap();
    root
}

/// What `docs check` makes of `expr` against baseline `head` under `root`.
fn reading(root: &Path, expr: &str) -> crate::docs_claims::Reading {
    let doc = invoke::load_baseline_in(root, "head").unwrap().unwrap();
    crate::docs_claims::evaluate(expr, &doc, &unmeasured::read(&doc).unwrap())
}

fn unmeasured_as(reading: crate::docs_claims::Reading, reason: Reason) -> bool {
    matches!(reading, crate::docs_claims::Reading::Unmeasured(r, _) if r == reason)
}

fn kept(root: &Path) -> Vec<String> {
    let doc = invoke::load_baseline_in(root, "head").unwrap().unwrap();
    doc["items"].as_object().unwrap().keys().cloned().collect()
}

#[test]
fn a_sharded_failure_with_no_listing_names_what_the_shard_owned() {
    let shard = invoke::Shard { index: 1, count: 2 };
    let owned = (0..)
        .map(|n| format!("soothfast::f{n}"))
        .find(|id| shard.owns(id))
        .unwrap();
    let root = stale("sharded-unlisted", &[("core", &[&owned])]);
    crate::baseline_merge::stamp_shard_in(
        &root,
        "head",
        shard,
        "core",
        std::slice::from_ref(&owned),
    )
    .unwrap();
    let common = CommonArgs {
        shard: Some(shard),
        ..core()
    };
    save_head(
        &root,
        "head",
        &Run::default(),
        &HeadRun::Measured,
        &common,
        None,
        1,
    )
    .unwrap();
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    assert_eq!(doc["items"], json!({}));
    assert_eq!(doc["shard"], "1/2");
    let entry = &unmeasured::read(&doc).unwrap()[0];
    assert_eq!(entry.items, std::slice::from_ref(&owned));
    assert!(unmeasured_as(
        reading(&root, &format!("{owned}.alloc.allocs <= 0")),
        Reason::GateFailed
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_sharded_failure_with_nothing_owned_covers_the_shard_s_unowned_items() {
    let shard = invoke::Shard { index: 1, count: 2 };
    let root = scratch("sharded-nothing");
    let common = CommonArgs {
        shard: Some(shard),
        ..core()
    };
    save_head(
        &root,
        "head",
        &Run::default(),
        &HeadRun::Measured,
        &common,
        None,
        1,
    )
    .unwrap();
    let entry =
        &unmeasured::read(&invoke::load_baseline_in(&root, "head").unwrap().unwrap()).unwrap()[0];
    assert!(entry.all_items && entry.shard == Some(shard) && entry.items.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// `mark_package` as an errored gate under `common`, with no listing, over a
/// workspace whose bench packages `crates` reads.
fn mark_unlisted(
    root: &Path,
    common: &CommonArgs,
    crates: Result<Vec<String>, String>,
) -> Result<(), String> {
    let reason = Reason::GateFailed;
    mark_package_with(root, "head", common, reason, "no bench", None, || crates)
}

#[test]
fn one_package_failing_keeps_another_s_numbers_though_their_ids_share_a_prefix() {
    let root = stale(
        "shared-prefix",
        &[("a", &["soothfast::a_item"]), ("b", &["soothfast::b_item"])],
    );
    let common = CommonArgs {
        pkg: Some("a".into()),
        ..CommonArgs::default()
    };
    mark_unlisted(&root, &common, Ok(vec!["a".into(), "b".into()])).unwrap();
    assert_eq!(kept(&root), ["soothfast::b_item"]);
    assert!(unmeasured_as(
        reading(&root, "soothfast::a_item.alloc.allocs <= 0"),
        Reason::GateFailed
    ));
    assert!(matches!(
        reading(&root, "soothfast::b_item.alloc.allocs <= 1"),
        crate::docs_claims::Reading::Decided(crate::docs_claims::Outcome::Holds(_))
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_gate_without_p_that_fails_marks_only_the_packages_it_ran() {
    let root = stale(
        "no-p-ran",
        &[("a", &["soothfast::x"]), ("c", &["soothfast::z"])],
    );
    mark_unlisted(
        &root,
        &CommonArgs::default(),
        Ok(vec!["a".into(), "b".into()]),
    )
    .unwrap();
    assert_eq!(kept(&root), ["soothfast::z"]);
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    let marked: Vec<String> = unmeasured::read(&doc)
        .unwrap()
        .into_iter()
        .map(|e| e.package)
        .collect();
    assert_eq!(marked, ["a", "b"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_metadata_failure_keeps_the_baseline_marks_its_owners_and_fails() {
    let root = stale("metadata-failed", &[("a", &["soothfast::x"])]);
    let mut doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    doc["items"]["jmh::bench"] = json!({ "imported": "jmh", "walltime": { "median_ns": 5.0 } });
    invoke::write_baseline_in(&root, "head", &doc).unwrap();
    let outcome = mark_unlisted(&root, &CommonArgs::default(), Err("no Cargo.toml".into()));
    assert!(outcome.unwrap_err().contains("no Cargo.toml"));
    assert_eq!(kept(&root), ["jmh::bench"]);
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    assert_eq!(unmeasured::read(&doc).unwrap()[0].package, "a");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_linked_member_s_items_in_the_listing_belong_to_the_package_that_listed_them() {
    let root = stale(
        "linked-member",
        &[("server", &["soothfast::own", "wire::parse"])],
    );
    let common = CommonArgs {
        pkg: Some("server".into()),
        ..CommonArgs::default()
    };
    let listing = ["soothfast::own".to_string(), "wire::parse".to_string()];
    let crates = || Ok(vec!["server".to_string()]);
    let reason = Reason::GateFailed;
    mark_package_with(
        &root,
        "head",
        &common,
        reason,
        "failed",
        Some(&listing),
        crates,
    )
    .unwrap();
    assert!(kept(&root).is_empty());
    assert!(unmeasured_as(
        reading(&root, "wire::parse.alloc.allocs <= 0"),
        Reason::GateFailed
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_passing_shard_whose_listing_failed_drops_its_numbers_and_fails() {
    let shard = invoke::Shard { index: 1, count: 2 };
    let owned = (0..)
        .map(|n| format!("soothfast::f{n}"))
        .find(|id| shard.owns(id))
        .unwrap();
    let root = stale("shard-listing-failed", &[("core", &[&owned])]);
    crate::baseline_merge::stamp_shard_in(
        &root,
        "head",
        shard,
        "core",
        std::slice::from_ref(&owned),
    )
    .unwrap();
    let common = CommonArgs {
        shard: Some(shard),
        ..core()
    };
    let run = run_of(vec![(owned.as_str(), item(None))]);
    let saved = save_head(&root, "head", &run, &HeadRun::Measured, &common, None, 0);
    assert!(saved.is_err());
    assert!(kept(&root).is_empty());
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    let entry = &unmeasured::read(&doc).unwrap()[0];
    assert!(entry.detail.contains("listing failed"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_passing_save_records_its_owners_and_drops_what_it_no_longer_lists() {
    let root = stale("save-owners", &[("core", &["soothfast::gone"])]);
    let run = run_of(vec![("soothfast::kept", item(None))]);
    let listed = ["soothfast::kept".to_string()];
    save_head(
        &root,
        "head",
        &run,
        &HeadRun::Measured,
        &core(),
        Some(&listed),
        0,
    )
    .unwrap();
    assert_eq!(kept(&root), ["soothfast::kept"]);
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    assert_eq!(unmeasured::owned_by(&doc, "core"), ["soothfast::kept"]);
    let _ = std::fs::remove_dir_all(&root);
}
