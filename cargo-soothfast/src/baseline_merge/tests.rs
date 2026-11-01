use super::*;

/// An id of package `core` that shard `index` of `count` owns.
fn owned_by(index: u64, count: u64) -> String {
    let shard = Shard { index, count };
    (0..)
        .map(|n| format!("core::f{n}"))
        .find(|id| shard.owns(id))
        .unwrap()
}

fn item() -> Value {
    json!({ "alloc": { "allocs": 0, "bytes": 0 } })
}

/// Shard `index` of 3's baseline: its own item, listing every leg's.
fn shard_file(index: u64) -> (String, Value) {
    let listed: Vec<String> = (1..=3).map(|i| owned_by(i, 3)).collect();
    let mut doc = json!({
        "version": 1,
        "shard": format!("{index}/3"),
        "listed": { "core": listed },
        "items": {},
    });
    doc["items"][owned_by(index, 3)] = item();
    (format!("shard-{index}.json"), doc)
}

fn packages() -> Vec<String> {
    vec!["core".to_string()]
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
    let mut doc = json!({ "version": 1, "items": {} });
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
    stamp_shard_in(&root, "head", first, &["core::a".into()]).unwrap();
    assert!(check_shard_in(&root, "head", first).is_ok());
    assert!(check_shard_in(&root, "head", second).is_err());
    let unsharded = json!({ "items": { "core::a": item() } });
    invoke::write_baseline_in(&root, "plain", &unsharded).unwrap();
    assert!(check_shard_in(&root, "plain", first).is_err());
    let _ = std::fs::remove_dir_all(&root);
}
