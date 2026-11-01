use super::*;

/// An id under the shared bench crate name that shard `index` of `count` owns.
fn owned_by(index: u64, count: u64) -> String {
    let shard = Shard { index, count };
    (0..)
        .map(|n| format!("soothfast::f{n}"))
        .find(|id| shard.owns(id))
        .unwrap()
}

fn item() -> Value {
    json!({ "alloc": { "allocs": 0, "bytes": 0 } })
}

/// Shard `index` of 3's baseline for package `core`: the item it owns,
/// recorded in `owners`, and the listing of every leg's.
fn shard_file(index: u64) -> (String, Value) {
    let listed: Vec<String> = (1..=3).map(|i| owned_by(i, 3)).collect();
    let own = owned_by(index, 3);
    let mut doc = json!({
        "version": 1,
        "shard": format!("{index}/3"),
        "listed": { "core": listed },
        "owners": { "core": [own] },
        "items": {},
    });
    doc["items"][own] = item();
    (format!("shard-{index}.json"), doc)
}

fn packages() -> Vec<String> {
    vec!["core".to_string()]
}

#[test]
fn a_shard_file_of_one_package_does_not_cover_another_though_ids_share_a_prefix() {
    let files: Vec<_> = (1..=3).map(shard_file).collect();
    let both = ["core".to_string(), "other".to_string()];
    let merged = merge(&files, 3, &both).unwrap();
    let entries = unmeasured::read(&merged).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].package, "other");
    assert_eq!(entries[0].reason, Reason::NoMeasurement);
    assert_eq!(merged["owners"]["core"].as_array().unwrap().len(), 3);
}

#[test]
fn every_shard_merges_into_one_measured_baseline() {
    let files: Vec<_> = (1..=3).map(shard_file).collect();
    let merged = merge(&files, 3, &packages()).unwrap();
    assert_eq!(items(&merged).unwrap().len(), 3);
    assert_eq!(merged["unmeasured"], json!([]));
    assert!(merged.get(SHARD_KEY).is_none() && merged.get(LISTED_KEY).is_none());
}

#[test]
fn a_missing_shard_names_the_items_it_owns() {
    let files = vec![shard_file(1), shard_file(3)];
    let merged = merge(&files, 3, &packages()).unwrap();
    let entries = unmeasured::read(&merged).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].reason, Reason::ShardMissing);
    assert_eq!(entries[0].items, [owned_by(2, 3)]);
    assert!(entries[0].detail.contains("shard 2/3"));
}

#[test]
fn a_package_no_leg_saved_names_no_item() {
    let merged = merge(&[], 1, &packages()).unwrap();
    let entries = unmeasured::read(&merged).unwrap();
    assert_eq!(entries[0].reason, Reason::NoMeasurement);
    assert!(entries[0].items.is_empty());
}

#[test]
fn a_file_of_another_split_is_a_count_error() {
    let err = merge(&[shard_file(1)], 2, &packages()).unwrap_err();
    assert!(matches!(err, MergeError::Count(_)));
}

#[test]
fn an_item_its_shard_does_not_own_is_corrupt() {
    let (label, mut doc) = shard_file(1);
    doc["items"][owned_by(2, 3)] = item();
    let err = merge(&[(label, doc)], 3, &packages()).unwrap_err();
    assert!(matches!(err, MergeError::Corrupt(_)));
}

#[test]
fn a_shard_saved_twice_or_a_bad_stamp_is_corrupt() {
    let twice = merge(&[shard_file(1), shard_file(1)], 3, &packages());
    assert!(matches!(twice, Err(MergeError::Corrupt(_))));
    let (label, mut doc) = shard_file(1);
    doc["shard"] = json!("first");
    let stamp = merge(&[(label, doc)], 3, &packages());
    assert!(matches!(stamp, Err(MergeError::Corrupt(_))));
}

#[test]
fn an_unsharded_baseline_is_shard_one_of_one() {
    let mut doc = json!({ "version": 1, "items": {}, "owners": { "core": ["core::a"] } });
    doc["items"]["core::a"] = item();
    let merged = merge(&[("head.json".into(), doc)], 1, &packages()).unwrap();
    assert_eq!(merged["unmeasured"], json!([]));
}

#[test]
fn a_failed_gate_s_stub_accounts_for_its_package_sharded_or_not() {
    let stub = |shard: Option<&str>| {
        let mut doc = json!({
            "version": 1,
            "items": {},
            "unmeasured": [{ "package": "core", "reason": "gate-failed", "detail": "failed", "items": ["core::a"] }],
        });
        if let Some(s) = shard {
            doc["shard"] = json!(s);
            doc["listed"] = json!({ "core": [] });
        }
        doc
    };
    let unsharded = merge(&[("head.json".into(), stub(None))], 1, &packages()).unwrap();
    let sharded = merge(&[("a.json".into(), stub(Some("1/1")))], 1, &packages()).unwrap();
    for merged in [unsharded, sharded] {
        let entries = unmeasured::read(&merged).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].reason, Reason::GateFailed);
    }
}

#[test]
fn an_imported_baseline_merges_beside_the_shards_without_counting_as_one() {
    let imported = json!({
        "version": 1,
        "items": { "jmh::Parse.run": { "imported": "jmh", "walltime": { "median_ns": 5.0 } } },
        "assertions": [],
    });
    let mut files: Vec<_> = (1..=3).map(shard_file).collect();
    files.push(("import.json".into(), imported));
    let merged = merge(&files, 3, &packages()).unwrap();
    assert_eq!(merged["items"]["jmh::Parse.run"]["imported"], "jmh");
    assert_eq!(merged["unmeasured"], json!([]));
}

#[test]
fn an_item_in_two_files_is_corrupt() {
    let imported = json!({ "items": { "jmh::a": { "imported": "jmh" } } });
    let twice = [
        ("a.json".into(), imported.clone()),
        ("b.json".into(), imported),
    ];
    assert!(matches!(merge(&twice, 1, &[]), Err(MergeError::Corrupt(_))));
}

#[test]
fn a_shard_stamp_refuses_another_shard_and_an_unsharded_run() {
    let root = std::env::temp_dir().join(format!("soothfast-stamp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let first = Shard { index: 1, count: 2 };
    let second = Shard { index: 2, count: 2 };
    assert!(check_shard_in(&root, "head", first).is_ok());
    stamp_shard_in(&root, "head", first, "core", &["core::a".into()]).unwrap();
    assert!(check_shard_in(&root, "head", first).is_ok());
    assert!(check_shard_in(&root, "head", second).is_err());
    let unsharded = json!({ "items": { "core::a": item() } });
    invoke::write_baseline_in(&root, "plain", &unsharded).unwrap();
    assert!(check_shard_in(&root, "plain", first).is_err());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn baseline_mark_on_a_broken_build_drops_the_old_numbers_and_still_fails() {
    let root = std::env::temp_dir().join(format!("soothfast-mark-broken-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let stale =
        json!({ "version": 1, "items": { "core::a": item() }, "owners": { "core": ["core::a"] } });
    invoke::write_baseline_in(&root, "head", &stale).unwrap();
    let common = invoke::CommonArgs {
        pkg: Some("core".into()),
        ..invoke::CommonArgs::default()
    };
    let rc = mark_listed(
        &root,
        "head",
        &common,
        Reason::NoParent,
        "no parent",
        Err("core's bench target did not build".into()),
    );
    assert_eq!(rc, 2);
    let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
    assert_eq!(doc["items"], json!({}));
    let entries = unmeasured::read(&doc).unwrap();
    assert!(entries[0].items == ["core::a"] && entries[0].reason == Reason::NoParent);
    let reading = crate::docs_claims::evaluate("core::a.alloc.allocs <= 0", &doc, &entries);
    assert!(matches!(
        reading,
        crate::docs_claims::Reading::Decided(crate::docs_claims::Outcome::NotEvaluated(
            Reason::NoParent,
            _
        ))
    ));
    let _ = std::fs::remove_dir_all(&root);
}

fn ungated<'a>(event: &'a str, imports: &'a [String]) -> Ungated<'a> {
    Ungated { event, imports }
}

#[test]
fn an_event_the_gate_skips_writes_one_entry_for_everything_but_imports() {
    let imports = ["jmh".to_string()];
    let event = ungated("workflow_dispatch", &imports);
    let merged = merge_on(&[], 1, &packages(), Some(&event)).unwrap();
    let entries = unmeasured::read(&merged).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].reason, Reason::NotGatedOnEvent);
    assert_eq!(entries[0].except_imported.as_deref(), Some(&imports[..]));
    assert!(entries[0].detail.contains("workflow_dispatch"));
}

#[test]
fn an_id_two_packages_own_across_files_is_refused() {
    let file = |package: &str, shard: &str| json!({ "version": 1, "shard": shard, "items": {}, "owners": { package: ["soothfast::same"] } });
    let files = [
        ("a.json".into(), file("a", "1/2")),
        ("b.json".into(), file("b", "2/2")),
    ];
    let Err(MergeError::Corrupt(why)) = merge(&files, 2, &[]) else {
        panic!("a shared id must be refused");
    };
    assert!(why.contains("listed by both a and b"));
}

/// Checks one claim on `core::f0` against `merged`, as an edited claim:
/// the merge-base never made it.
fn edited_claim_failures(merged: &Value) -> u32 {
    let doc = markdown_claim("core::f0.alloc.allocs <= 0");
    let never_made = || Ok(crate::docs_claims::Claims::new());
    crate::docs_claims::check(&[("a.md", &doc)], Some(merged), never_made).unwrap()
}

fn markdown_claim(expr: &str) -> soothfast_docs::markdown::Doc {
    soothfast_docs::markdown::scan(&format!("<!-- soothfast:claim {expr} -->\n")).unwrap()
}

#[test]
fn an_edited_claim_on_a_merge_group_event_is_not_evaluated() {
    let event = ungated("merge_group", &[]);
    let merged = merge_on(&[], 1, &packages(), Some(&event)).unwrap();
    assert_eq!(edited_claim_failures(&merged), 0);
}

/// A gate's baseline of `core` that names `core::f0` for `reason`.
fn stub(reason: &str) -> Value {
    json!({
        "version": 1,
        "items": {},
        "owners": { "core": ["core::f0"] },
        "unmeasured": [{ "package": "core", "reason": reason, "detail": reason,
                         "items": ["core::f0"] }],
    })
}

#[test]
fn a_claim_on_a_parentless_commit_with_no_base_ref_is_not_evaluated() {
    let merged = merge(&[("head.json".into(), stub("no-parent"))], 1, &packages()).unwrap();
    assert_eq!(edited_claim_failures(&merged), 0);
}

#[test]
fn an_edited_claim_a_failed_gate_could_have_measured_still_fails() {
    let merged = merge(&[("head.json".into(), stub("gate-failed"))], 1, &packages()).unwrap();
    assert_eq!(edited_claim_failures(&merged), 1);
}

#[test]
fn an_ungated_event_still_names_a_covered_package_s_missing_shard() {
    let event = ungated("push", &[]);
    let files = [shard_file(1), shard_file(3)];
    let merged = merge_on(&files, 3, &packages(), Some(&event)).unwrap();
    let entries = unmeasured::read(&merged).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].reason, Reason::ShardMissing);
    assert_eq!(entries[0].items, [owned_by(2, 3)]);
    assert_eq!(entries[1].reason, Reason::NotGatedOnEvent);
}
