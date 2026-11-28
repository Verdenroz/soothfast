//! Storing the passes of a default-branch run.

use std::collections::BTreeSet;

use serde_json::Value;

use super::slice;
use super::store::{Pass, Store, is_key};

/// A baseline file of the run: its file stem, which names an import lane, and
/// its document.
pub type Baseline = (String, Value);

/// Stores `pass` for each lane of `listing` (the plan's `lanes.json`) whose
/// every result is among `passed`, with the part of `baselines` its gate
/// measured. Returns how many it stored.
pub fn save(
    listing: &str,
    passed: &BTreeSet<String>,
    baselines: &[Baseline],
    store: &Store,
    pass: &Pass,
) -> Result<usize, String> {
    let lanes: Vec<Value> =
        serde_json::from_str(listing).map_err(|e| format!("lanes.json: {e}"))?;
    let mut entries = Vec::with_capacity(lanes.len());
    for lane in &lanes {
        let (Some(id), Some(key), Some(records)) = (
            lane["lane"].as_str(),
            lane["key"].as_str(),
            lane["records"].as_array(),
        ) else {
            return Err("lanes.json: an entry lacks its lane, key or records".into());
        };
        if !is_key(key) {
            return Err(format!("lanes.json: {key:?} is not a lane key"));
        }
        entries.push((id, key, records));
    }
    let mut saved = 0;
    for (id, key, records) in entries {
        let complete = !records.is_empty()
            && records
                .iter()
                .all(|r| r.as_str().is_some_and(|r| passed.contains(r)));
        if complete {
            let baseline = measured(id, records.len(), baselines);
            store.record(
                id,
                key,
                &Pass {
                    baseline,
                    ..pass.clone()
                },
            );
            saved += 1;
        }
    }
    Ok(saved)
}

/// What the lane `id`, whose gate left `legs` results, measured.
fn measured(id: &str, legs: usize, baselines: &[Baseline]) -> Option<Value> {
    if let Some(package) = id.strip_prefix("rust:") {
        let docs: Vec<&Value> = baselines.iter().map(|(_, doc)| doc).collect();
        return slice::rust(package, &docs, legs);
    }
    let name = id.strip_prefix("import:")?;
    let stem = format!("import-{name}");
    baselines
        .iter()
        .find(|(s, _)| *s == stem)
        .map(|(_, doc)| doc.clone())
}

#[cfg(test)]
mod tests {
    use super::super::store::key;
    use super::*;
    use crate::test::fixture::TempDir;
    use crate::verdict::{plain_passes, write_carried};
    use serde_json::json;
    use std::path::Path;

    fn put(dir: &Path, name: &str, verdict: &str, extra: Value) {
        let mut doc = json!({
            "name": name, "kind": "rust", "verdict": verdict, "tail": "", "seconds": 1,
        });
        doc.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let file = name.replace('/', "-");
        std::fs::write(dir.join(format!("{file}.json")), doc.to_string()).unwrap();
    }

    fn listing(lanes: &[(&str, &[&str])]) -> String {
        let lanes: Vec<Value> = lanes
            .iter()
            .map(|(lane, records)| json!({"lane": lane, "key": lane_key(lane), "records": records, "kind": "rust"}))
            .collect();
        Value::Array(lanes).to_string()
    }

    fn lane_key(lane: &str) -> String {
        key(lane, "fingerprint", "")
    }

    fn pass() -> Pass {
        Pass {
            commit: "abc".into(),
            run: "9".into(),
            baseline: None,
        }
    }

    fn stored(store: &Store, lane: &str) -> bool {
        store.lookup(lane, &lane_key(lane)).is_some()
    }

    #[test]
    fn only_a_lane_whose_every_result_passed_is_stored() {
        let results = TempDir::new("lane-save-results");
        let dir = TempDir::new("lane-save-store");
        std::fs::create_dir_all(&*results).unwrap();
        std::fs::create_dir_all(&*dir).unwrap();
        put(&results, "core", "pass", json!({}));
        put(&results, "cli", "regressed", json!({}));
        put(&results, "spec-core", "nothing-to-compare", json!({}));
        let store = Store::at(dir.to_path_buf());
        let lanes = [
            ("rust:core", &["core"][..]),
            ("rust:cli", &["cli"]),
            ("check:spec-core", &["spec-core"]),
            ("rust:gone", &["gone"]),
            ("rust:both", &["core", "cli"]),
            ("rust:none", &[]),
        ];
        let saved = save(
            &listing(&lanes),
            &plain_passes(&results),
            &[],
            &store,
            &pass(),
        )
        .unwrap();
        assert_eq!(saved, 1);
        assert!(stored(&store, "rust:core"));
        for lane in [
            "rust:cli",
            "check:spec-core",
            "rust:gone",
            "rust:both",
            "rust:none",
        ] {
            assert!(!stored(&store, lane), "{lane}");
        }
    }

    #[test]
    fn a_sharded_lane_needs_every_leg() {
        let results = TempDir::new("lane-save-shards");
        let dir = TempDir::new("lane-save-shards-store");
        std::fs::create_dir_all(&*results).unwrap();
        std::fs::create_dir_all(&*dir).unwrap();
        put(&results, "core#1/2", "pass", json!({}));
        let store = Store::at(dir.to_path_buf());
        let lanes = listing(&[("rust:core", &["core#1/2", "core#2/2"])]);
        assert_eq!(
            save(&lanes, &plain_passes(&results), &[], &store, &pass()),
            Ok(0)
        );
        put(&results, "core#2/2", "pass", json!({}));
        assert_eq!(
            save(&lanes, &plain_passes(&results), &[], &store, &pass()),
            Ok(1)
        );
        assert!(stored(&store, "rust:core"));
    }

    #[test]
    fn unevaluated_and_carried_results_are_not_passes() {
        let results = TempDir::new("lane-save-skips");
        let dir = TempDir::new("lane-save-skips-store");
        std::fs::create_dir_all(&*results).unwrap();
        std::fs::create_dir_all(&*dir).unwrap();
        put(&results, "docs", "pass", json!({"unevaluated": true}));
        let carried = results.join("carried.json");
        write_carried("core".into(), "rust".into(), "abc", &carried).unwrap();
        let store = Store::at(dir.to_path_buf());
        let lanes = listing(&[("docs", &["docs"]), ("rust:core", &["core"])]);
        assert_eq!(
            save(&lanes, &plain_passes(&results), &[], &store, &pass()),
            Ok(0)
        );
    }

    #[test]
    fn a_stored_pass_reads_back_with_its_commit_and_run() {
        let results = TempDir::new("lane-save-read");
        let dir = TempDir::new("lane-save-read-store");
        std::fs::create_dir_all(&*results).unwrap();
        std::fs::create_dir_all(&*dir).unwrap();
        put(&results, "core", "pass", json!({}));
        let store = Store::at(dir.to_path_buf());
        let lanes = listing(&[("rust:core", &["core"])]);
        save(&lanes, &plain_passes(&results), &[], &store, &pass()).unwrap();
        assert_eq!(
            store.lookup("rust:core", &lane_key("rust:core")),
            Some(pass())
        );
    }

    fn leg(owned: &[(&str, &[&str])]) -> Value {
        let ids = owned.iter().flat_map(|(_, ids)| ids.iter());
        let items: serde_json::Map<String, Value> = ids
            .map(|id| (id.to_string(), json!({"perfcnt": {"instructions": 1}})))
            .collect();
        let owners: serde_json::Map<String, Value> = owned
            .iter()
            .map(|(p, ids)| (p.to_string(), json!(ids)))
            .collect();
        json!({"version": 1, "items": items, "owners": owners})
    }

    #[test]
    fn a_stored_lane_keeps_the_items_its_legs_measured() {
        let results = TempDir::new("lane-save-baseline");
        let dir = TempDir::new("lane-save-baseline-store");
        std::fs::create_dir_all(&*results).unwrap();
        std::fs::create_dir_all(&*dir).unwrap();
        for name in ["core#1/2", "core#2/2", "cli#1/2", "cli#2/2", "node"] {
            put(&results, name, "pass", json!({}));
        }
        let store = Store::at(dir.to_path_buf());
        let lanes = listing(&[
            ("rust:core", &["core#1/2", "core#2/2"]),
            ("rust:cli", &["cli#1/2", "cli#2/2"]),
            ("import:node", &["node"]),
        ]);
        let one = leg(&[("core", &["core::a"]), ("cli", &["cli::x"])]);
        let two = leg(&[("core", &["core::b"]), ("cli", &[])]);
        let node = json!({"version": 1, "imported": "node", "items": {"node::t": {}}});
        let baselines = [
            ("head".to_string(), one),
            ("head".to_string(), two),
            ("import-node".to_string(), node.clone()),
        ];
        let passed = plain_passes(&results);
        assert_eq!(save(&lanes, &passed, &baselines, &store, &pass()), Ok(3));
        let baseline = |lane: &str| store.lookup(lane, &lane_key(lane)).unwrap().baseline;
        let core = baseline("rust:core").unwrap();
        assert_eq!(core["owners"]["core"], json!(["core::a", "core::b"]));
        assert!(core["items"].get("cli::x").is_none());
        assert!(
            baseline("rust:cli").unwrap()["items"]
                .get("cli::x")
                .is_some()
        );
        assert_eq!(baseline("import:node"), Some(node));
    }

    #[test]
    fn a_lane_missing_a_leg_or_ownership_is_stored_without_a_baseline() {
        let results = TempDir::new("lane-save-partial");
        let dir = TempDir::new("lane-save-partial-store");
        std::fs::create_dir_all(&*results).unwrap();
        std::fs::create_dir_all(&*dir).unwrap();
        for name in ["core#1/2", "core#2/2", "cli"] {
            put(&results, name, "pass", json!({}));
        }
        let store = Store::at(dir.to_path_buf());
        let lanes = listing(&[
            ("rust:core", &["core#1/2", "core#2/2"]),
            ("rust:cli", &["cli"]),
        ]);
        let one = leg(&[("core", &["core::a"])]);
        let old = json!({"version": 1, "items": {"cli::x": {}}});
        let baselines = [("head".to_string(), one), ("head".to_string(), old)];
        let passed = plain_passes(&results);
        assert_eq!(save(&lanes, &passed, &baselines, &store, &pass()), Ok(2));
        for lane in ["rust:core", "rust:cli"] {
            assert_eq!(store.lookup(lane, &lane_key(lane)).unwrap().baseline, None);
        }
    }

    #[test]
    fn a_key_that_is_not_a_digest_is_refused_before_anything_is_written() {
        let parent = TempDir::new("lane-save-escape");
        let dir = parent.join("lanes");
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::at(dir);
        let passed = BTreeSet::from(["core".to_string()]);
        let lanes = r#"[{"lane":"rust:core","key":"../x","records":["core"]}]"#;
        assert!(save(lanes, &passed, &[], &store, &pass()).is_err());
        assert!(!parent.join("x").exists());
    }

    #[test]
    fn a_malformed_listing_is_an_error() {
        let dir = TempDir::new("lane-save-bad");
        std::fs::create_dir_all(&*dir).unwrap();
        let store = Store::at(dir.to_path_buf());
        let passed = BTreeSet::new();
        assert!(save("{", &passed, &[], &store, &pass()).is_err());
        assert!(save("[{\"lane\":\"a\"}]", &passed, &[], &store, &pass()).is_err());
    }
}
