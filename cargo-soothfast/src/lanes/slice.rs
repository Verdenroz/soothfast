//! The part of a run's baselines that one gate lane measured.

use serde_json::{Map, Value, json};

use crate::unmeasured::{self, OWNERS_KEY};

/// What the `legs` of a gate (one baseline per shard) measured of `package`,
/// as a baseline of its own. `None` unless exactly `expect` legs account for
/// the package by their owners, since a missing leg would carry a partial run,
/// and `None` when a leg holds an assertion with no id, which no package can
/// claim.
pub fn rust(package: &str, legs: &[&Value], expect: usize) -> Option<Value> {
    let owning: Vec<&&Value> = legs
        .iter()
        .filter(|leg| leg[OWNERS_KEY][package].is_array())
        .collect();
    let idless = |a: &Value| a["id"].as_str().is_none_or(str::is_empty);
    let unattributable = owning.iter().any(|leg| {
        leg["assertions"]
            .as_array()
            .into_iter()
            .flatten()
            .any(idless)
    });
    if owning.len() != expect || unattributable {
        return None;
    }
    let mut items = Map::new();
    let mut owned = Vec::new();
    let mut entries = Vec::new();
    let mut assertions = Vec::new();
    for leg in &owning {
        let ids = unmeasured::owned_by(leg, package);
        for id in &ids {
            if let Some(item) = leg["items"].get(id) {
                items.insert(id.clone(), item.clone());
            }
        }
        for a in leg["assertions"].as_array().into_iter().flatten() {
            if a["id"]
                .as_str()
                .is_some_and(|id| ids.iter().any(|o| o == id))
            {
                assertions.push(a.clone());
            }
        }
        entries.extend(
            unmeasured::read(leg)
                .ok()?
                .iter()
                .filter(|e| e.package == package)
                .map(unmeasured::Unmeasured::to_json),
        );
        owned.extend(ids);
    }
    let mut slice = json!({
        "version": 1,
        "items": items,
        "assertions": assertions,
        "unmeasured": entries,
        OWNERS_KEY: { package: owned },
    });
    for key in ["noise_pct", "build", "created_unix"] {
        if let Some(v) = owning.iter().find_map(|leg| leg.get(key)) {
            slice[key] = v.clone();
        }
    }
    Some(slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leg(owned: &[(&str, &[&str])], measured: &[&str]) -> Value {
        let items: Map<String, Value> = measured
            .iter()
            .map(|id| (id.to_string(), json!({"perfcnt": {"instructions": 1}})))
            .collect();
        let owners: Map<String, Value> = owned
            .iter()
            .map(|(p, ids)| (p.to_string(), json!(ids)))
            .collect();
        json!({"version": 1, "items": items, "owners": owners, "noise_pct": 2.0})
    }

    #[test]
    fn a_package_keeps_only_its_own_items_from_every_leg() {
        let a = leg(
            &[("core", &["core::a"]), ("cli", &["cli::x"])],
            &["core::a", "cli::x"],
        );
        let b = leg(&[("core", &["core::b"]), ("cli", &[])], &["core::b"]);
        let slice = rust("core", &[&a, &b], 2).unwrap();
        let ids: Vec<&String> = slice["items"].as_object().unwrap().keys().collect();
        assert_eq!(ids, ["core::a", "core::b"]);
        assert_eq!(slice["owners"]["core"], json!(["core::a", "core::b"]));
        assert!(slice["owners"].get("cli").is_none());
        assert_eq!(slice["noise_pct"], 2.0);
    }

    #[test]
    fn a_missing_leg_or_missing_ownership_gives_no_slice() {
        let a = leg(&[("core", &["core::a"])], &["core::a"]);
        assert_eq!(rust("core", &[&a], 2), None);
        let old = json!({"version": 1, "items": {"core::a": {}}});
        assert_eq!(rust("core", &[&old], 1), None);
        assert_eq!(rust("cli", &[&a], 1), None);
    }

    #[test]
    fn an_assertion_with_no_id_gives_no_slice() {
        let mut a = leg(&[("core", &["core::a"])], &["core::a"]);
        a["assertions"] = json!([{"kind": "alloc", "detail": "d", "ok": true}]);
        assert_eq!(rust("core", &[&a], 1), None);
    }

    #[test]
    fn assertions_and_unmeasured_entries_follow_their_package() {
        let mut a = leg(
            &[("core", &["core::a"]), ("cli", &["cli::x"])],
            &["core::a"],
        );
        a["assertions"] = json!([
            {"id": "core::a", "ok": true},
            {"id": "cli::x", "ok": false},
        ]);
        a["unmeasured"] = json!([
            {"package": "cli", "reason": "no-measurement", "detail": "d", "items": ["cli::x"]},
        ]);
        let slice = rust("core", &[&a], 1).unwrap();
        assert_eq!(slice["assertions"], json!([{"id": "core::a", "ok": true}]));
        assert_eq!(slice["unmeasured"], json!([]));
        let cli = rust("cli", &[&a], 1).unwrap();
        assert_eq!(cli["unmeasured"][0]["package"], "cli");
        assert_eq!(cli["assertions"], json!([{"id": "cli::x", "ok": false}]));
    }
}
