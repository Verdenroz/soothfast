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
    let listed = [("core::a".to_string(), None), ("core::b".to_string(), None)];
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
    crate::docs_claims::Against::new(&doc)
        .unwrap()
        .evaluate(expr)
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
fn a_failed_package_with_no_listing_names_nothing_and_its_claims_fail() {
    let root = scratch("unlisted-nothing");
    save_head(
        &root,
        "head",
        &Run::default(),
        &HeadRun::Measured,
        &core(),
        None,
        1,
    )
    .unwrap();
    let entry =
        &unmeasured::read(&invoke::load_baseline_in(&root, "head").unwrap().unwrap()).unwrap()[0];
    assert!(entry.items.is_empty() && entry.detail.contains("no listing"));
    let Reading::Decided(Outcome::Failed(why)) = reading(&root, "soothfast::x.alloc.allocs <= 0")
    else {
        panic!("a claim on an item nobody named must fail");
    };
    assert!(why.contains("no listing"));
    let _ = std::fs::remove_dir_all(&root);
}

use crate::docs_claims::{Outcome, Reading};

/// `mark_package` as an errored gate under `common`, with no listing.
fn mark_unlisted(root: &Path, common: &CommonArgs) -> Result<(), String> {
    mark_package(root, "head", common, Reason::GateFailed, "no bench", None)
}

fn package(pkg: &str) -> CommonArgs {
    CommonArgs {
        pkg: Some(pkg.into()),
        ..CommonArgs::default()
    }
}

#[test]
fn one_package_failing_keeps_another_s_numbers_though_their_ids_share_a_prefix() {
    let root = stale(
        "shared-prefix",
        &[("a", &["soothfast::a_item"]), ("b", &["soothfast::b_item"])],
    );
    mark_unlisted(&root, &package("a")).unwrap();
    assert_eq!(kept(&root), ["soothfast::b_item"]);
    assert!(unmeasured_as(
        reading(&root, "soothfast::a_item.alloc.allocs <= 0"),
        Reason::GateFailed
    ));
    assert!(matches!(
        reading(&root, "soothfast::b_item.alloc.allocs <= 1"),
        Reading::Decided(Outcome::Holds(_))
    ));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_save_without_p_in_a_multi_package_workspace_is_refused() {
    let crates = || Ok(vec!["a".to_string(), "b".to_string()]);
    let err = save_package(&CommonArgs::default(), crates).unwrap_err();
    assert!(err.contains("needs -p") && err.contains("a, b"));
}

#[test]
fn a_save_without_p_in_a_single_package_workspace_is_that_package_s() {
    let crates = || Ok(vec!["only".to_string()]);
    assert_eq!(
        save_package(&CommonArgs::default(), crates).unwrap(),
        "only"
    );
    let given = || -> Result<Vec<String>, String> { panic!("-p needs no lookup") };
    assert_eq!(save_package(&package("given"), given).unwrap(), "given");
}

#[test]
fn a_save_without_p_whose_workspace_cannot_be_read_is_refused() {
    let crates = || Err("no Cargo.toml".to_string());
    let err = save_package(&CommonArgs::default(), crates).unwrap_err();
    assert!(err.contains("no Cargo.toml"));
}

#[test]
fn an_errored_gate_without_p_writes_nothing() {
    let root = stale("errored-no-p", &[("a", &["soothfast::x"])]);
    save_errored(&root, "head", &CommonArgs::default(), None, "no config").unwrap();
    assert_eq!(kept(&root), ["soothfast::x"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_linked_member_s_items_in_the_listing_belong_to_the_package_that_listed_them() {
    let root = stale(
        "linked-member",
        &[("server", &["soothfast::own", "wire::parse"])],
    );
    let listing = ["soothfast::own".to_string(), "wire::parse".to_string()];
    let reason = Reason::GateFailed;
    mark_package(
        &root,
        "head",
        &package("server"),
        reason,
        "failed",
        Some(&listing),
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
    let only = std::slice::from_ref(&owned);
    crate::baseline_merge::stamp_shard_in(&root, "head", shard, "core", only).unwrap();
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
    let listed = [("soothfast::kept".to_string(), None)];
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

/// Save `ids` measured for `scope` into the `head` baseline under `root`.
fn save_ids(root: &Path, ids: &[&str], scope: invoke::SaveScope) -> std::io::Result<()> {
    let run = run_of(ids.iter().map(|id| (*id, item(None))).collect());
    invoke::save_baseline_in(root, "head", &run, scope).map(|_| ())
}

#[test]
fn measure_p_drops_an_item_its_package_renamed() {
    let root = stale("measure-renamed", &[("core", &["soothfast::old_name"])]);
    let scope = invoke::SaveScope::BenchFull(Some("core".into()));
    save_ids(&root, &["soothfast::new_name"], scope).unwrap();
    assert_eq!(kept(&root), ["soothfast::new_name"]);
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    assert_eq!(unmeasured::owned_by(&doc, "core"), ["soothfast::new_name"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_filtered_save_keeps_the_unfiltered_numbers() {
    let root = stale(
        "filtered-save",
        &[("core", &["soothfast::filtered", "soothfast::other"])],
    );
    let scope = invoke::SaveScope::BenchFiltered(Some("core".into()));
    save_ids(&root, &["soothfast::filtered"], scope).unwrap();
    assert_eq!(kept(&root), ["soothfast::filtered", "soothfast::other"]);
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    assert_eq!(
        unmeasured::owned_by(&doc, "core"),
        ["soothfast::filtered", "soothfast::other"]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_save_of_an_id_another_package_owns_fails_its_claims_as_ambiguous() {
    let root = stale("duplicate-id", &[("a", &["soothfast::same"])]);
    let scope = invoke::SaveScope::BenchFull(Some("b".into()));
    save_ids(&root, &["soothfast::same"], scope).unwrap();
    assert!(kept(&root).is_empty());
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    let side = doc["ambiguous"]["soothfast::same"].as_array().unwrap();
    let writers: Vec<&Value> = side.iter().map(|s| &s["package"]).collect();
    assert_eq!(writers, [&json!("a"), &json!("b")]);
    assert_eq!(unmeasured::owned_by(&doc, "a"), ["soothfast::same"]);
    assert_eq!(unmeasured::owned_by(&doc, "b"), ["soothfast::same"]);
    let Reading::Decided(Outcome::Failed(why)) =
        reading(&root, "soothfast::same.alloc.allocs <= 1")
    else {
        panic!("a claim on an ambiguous id must fail");
    };
    assert!(why.contains("measured by both a and b; a claim on it is ambiguous"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_item_that_moved_keeps_its_number_through_both_packages_saves() {
    let root = stale("moved-item", &[("a", &["soothfast::moved"])]);
    let b = invoke::SaveScope::BenchFull(Some("b".into()));
    save_ids(&root, &["soothfast::moved"], b).unwrap();
    let a = invoke::SaveScope::BenchFull(Some("a".into()));
    save_ids(&root, &[], a).unwrap();
    assert_eq!(kept(&root), ["soothfast::moved"]);
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    assert_eq!(unmeasured::owned_by(&doc, "b"), ["soothfast::moved"]);
    let holds = reading(&root, "soothfast::moved.walltime.median_ns < 1ms");
    assert!(matches!(holds, Reading::Decided(Outcome::Holds(_))));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_full_save_into_a_baseline_with_no_owners_replaces_its_bench_items() {
    let root = scratch("legacy-full-save");
    let legacy = json!({ "version": 1, "items": {
        "core::old_name": { "alloc": { "allocs": 1, "bytes": 8 } },
        "jmh::kept": { "imported": "jmh", "walltime": { "median_ns": 5.0 } },
    } });
    invoke::write_baseline_in(&root, "head", &legacy).unwrap();
    let scope = invoke::SaveScope::BenchFull(Some("core".into()));
    save_ids(&root, &["soothfast::new_name"], scope).unwrap();
    assert_eq!(kept(&root), ["jmh::kept", "soothfast::new_name"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_failed_filtered_gate_drops_and_names_only_the_filtered_ids() {
    let root = stale(
        "filtered-failure",
        &[("core", &["soothfast::parse_fast", "soothfast::other"])],
    );
    let common = CommonArgs {
        filter: Some("parse".into()),
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
    assert_eq!(kept(&root), ["soothfast::other"]);
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    assert_eq!(
        unmeasured::read(&doc).unwrap()[0].items,
        ["soothfast::parse_fast"]
    );
    assert_eq!(
        unmeasured::owned_by(&doc, "core"),
        ["soothfast::parse_fast", "soothfast::other"]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_buildcost_save_still_needs_a_package() {
    let root = scratch("buildcost-save");
    let common = CommonArgs {
        backend: Some("buildcost".into()),
        ..CommonArgs::default()
    };
    let run = run_of(vec![("buildcost::core::default", item(None))]);
    let saved = save_head(&root, "head", &run, &HeadRun::Measured, &common, None, 0);
    assert_eq!(saved, Err("--save-baseline needs -p PKG".into()));
    assert!(!invoke::baseline_path(&root, "head").exists());
}

#[test]
fn a_buildcost_save_without_a_package_is_refused_before_anything_is_built() {
    let mut common = CommonArgs {
        backend: Some("buildcost".into()),
        ..CommonArgs::default()
    };
    let crates = |_: &CommonArgs| -> Result<Vec<String>, String> { panic!("not consulted") };
    let configured = configure_save(&mut common, true, crates, |_| Ok::<(), String>(()));
    assert_eq!(
        configured,
        Err("--backend buildcost requires -p PKG".into())
    );
    common.pkg = Some("core".into());
    let configured = configure_save(&mut common, true, crates, |_| Ok::<(), String>(()));
    assert_eq!(configured, Ok(Ok(())));
}

#[test]
fn a_config_error_still_has_the_save_s_package_to_mark() {
    let mut common = CommonArgs::default();
    let only = |_: &CommonArgs| Ok(vec!["core".to_string()]);
    let configured = configure_save(&mut common, true, only, |c| {
        assert_eq!(c.pkg.as_deref(), Some("core"));
        Err::<(), _>("bad soothfast.toml".to_string())
    });
    assert_eq!(configured, Ok(Err("bad soothfast.toml".into())));
    let root = scratch("config-error");
    save_errored(&root, "head", &common, None, "bad soothfast.toml").unwrap();
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    let entries = unmeasured::read(&doc).unwrap();
    assert!(entries[0].package == "core" && entries[0].reason == Reason::GateFailed);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_shard_saves_only_its_own_ids_whatever_the_run_holds() {
    let root = scratch("shard-backstop");
    let shard = invoke::Shard { index: 1, count: 2 };
    let ids: Vec<String> = (0..8).map(|n| format!("core::f{n}")).collect();
    let run = run_of(ids.iter().map(|id| (id.as_str(), item(None))).collect());
    let common = CommonArgs {
        shard: Some(shard),
        ..core()
    };
    let listing: Vec<invoke::Listed> = ids.iter().map(|id| (id.clone(), None)).collect();
    let source = HeadRun::Measured;
    save_head(&root, "head", &run, &source, &common, Some(&listing), 0).unwrap();
    let saved = kept(&root);
    assert!(!saved.is_empty() && saved.iter().all(|id| shard.owns(id)));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_gate_that_failed_only_on_regressions_saves_head_s_numbers() {
    let run = run_of(vec![("core::a", item(None))]);
    let doc = saved("regressed", &run, &HeadRun::Measured, 0);
    assert!(doc["items"]["core::a"]["alloc"].is_object());
    assert!(unmeasured::read(&doc).unwrap().is_empty());
}
