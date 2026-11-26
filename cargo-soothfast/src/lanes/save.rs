//! Storing the passes of a default-branch run.

use std::collections::BTreeSet;

use serde_json::Value;

use super::store::{Pass, Store, is_key};

/// Stores `pass` for each lane of `listing` (the plan's `lanes.json`) whose
/// every result is among `passed`. Returns how many it stored.
pub fn save(
    listing: &str,
    passed: &BTreeSet<String>,
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
            store.record(id, key, pass);
            saved += 1;
        }
    }
    Ok(saved)
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
        let saved = save(&listing(&lanes), &plain_passes(&results), &store, &pass()).unwrap();
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
            save(&lanes, &plain_passes(&results), &store, &pass()),
            Ok(0)
        );
        put(&results, "core#2/2", "pass", json!({}));
        assert_eq!(
            save(&lanes, &plain_passes(&results), &store, &pass()),
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
            save(&lanes, &plain_passes(&results), &store, &pass()),
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
        save(&lanes, &plain_passes(&results), &store, &pass()).unwrap();
        assert_eq!(
            store.lookup("rust:core", &lane_key("rust:core")),
            Some(pass())
        );
    }

    #[test]
    fn a_key_that_is_not_a_digest_is_refused_before_anything_is_written() {
        let parent = TempDir::new("lane-save-escape");
        let dir = parent.join("lanes");
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::at(dir);
        let passed = BTreeSet::from(["core".to_string()]);
        let lanes = r#"[{"lane":"rust:core","key":"../x","records":["core"]}]"#;
        assert!(save(lanes, &passed, &store, &pass()).is_err());
        assert!(!parent.join("x").exists());
    }

    #[test]
    fn a_malformed_listing_is_an_error() {
        let dir = TempDir::new("lane-save-bad");
        std::fs::create_dir_all(&*dir).unwrap();
        let store = Store::at(dir.to_path_buf());
        let passed = BTreeSet::new();
        assert!(save("{", &passed, &store, &pass()).is_err());
        assert!(save("[{\"lane\":\"a\"}]", &passed, &store, &pass()).is_err());
    }
}
