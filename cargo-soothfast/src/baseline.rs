//! `cargo soothfast baseline merge --into NAME [--expect N] [--packages P...] FILE...`: fold the
//! partial baselines a sharded gate saved, one per shard, into one baseline.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::invoke;

/// Exit code for the wrong number of shard files, which a caller treats as
/// missing shards and not as a failure.
const MISSING: i32 = 3;

pub fn run(args: &[String]) -> i32 {
    match merge_args(args) {
        Ok(path) => {
            println!("baseline saved: {}", path.display());
            0
        }
        Err(Failure::Missing(e)) => {
            eprintln!("baseline: {e}");
            MISSING
        }
        Err(Failure::Other(e)) => {
            eprintln!("baseline: {e}");
            2
        }
    }
}

enum Failure {
    Missing(String),
    Other(String),
}

impl From<String> for Failure {
    fn from(e: String) -> Self {
        Failure::Other(e)
    }
}

fn merge_args(args: &[String]) -> Result<PathBuf, Failure> {
    const USAGE: &str =
        "usage: baseline merge --into NAME [--expect N] [--packages \"A B\"] FILE...";
    let (Some("merge"), rest) = (
        args.first().map(String::as_str),
        args.get(1..).unwrap_or(&[]),
    ) else {
        return Err(USAGE.to_string().into());
    };
    let (mut into, mut expect, mut files, mut packages) = (None, None, Vec::new(), Vec::new());
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--into" => into = it.next().cloned(),
            "--packages" => {
                let list = it.next().ok_or("--packages needs a list".to_string())?;
                packages.extend(list.split_whitespace().map(|p| p.replace('-', "_")));
            }
            "--expect" => {
                expect = Some(
                    it.next()
                        .and_then(|n| n.parse::<usize>().ok())
                        .ok_or("--expect needs a count".to_string())?,
                )
            }
            file => files.push(file.to_string()),
        }
    }
    let into = into.ok_or(USAGE.to_string())?;
    if let Some(n) = expect
        && files.len() != n
    {
        return Err(Failure::Missing(format!(
            "expected {n} shard baselines, found {}",
            files.len()
        )));
    }
    if files.is_empty() {
        return Err(USAGE.to_string().into());
    }
    let docs = files
        .iter()
        .map(|f| {
            let text = std::fs::read_to_string(Path::new(f)).map_err(|e| format!("{f}: {e}"))?;
            serde_json::from_str(&text).map_err(|e| format!("{f}: {e}"))
        })
        .collect::<Result<Vec<Value>, String>>()?;
    let existing = invoke::load_baseline(&into).map_err(|e| e.to_string())?;
    let merged = merge(&docs, existing, &packages)?;
    Ok(invoke::write_baseline(&into, &merged).map_err(|e| e.to_string())?)
}

/// The shards' baselines as one. They replace the bench items of every
/// package they cover in `existing`, so an item that no longer exists drops
/// out, and `existing` keeps its imported and `buildcost::` items. Shards own
/// disjoint items; the scalar fields take the newest or the noisiest, the
/// most cautious reading of any shard. The shards' build stamp replaces the
/// existing one, and they must agree on it. A listed package no shard
/// measured, or one a shard reports as `unmeasured`, is noted as unmeasured
/// and its stale items are dropped.
pub fn merge(
    docs: &[Value],
    existing: Option<Value>,
    packages: &[String],
) -> Result<Value, String> {
    let mut out = existing.unwrap_or_else(|| json!({ "version": 1 }));
    let mut covered: Vec<String> = docs
        .iter()
        .filter_map(|d| d["items"].as_object())
        .flatten()
        .filter(|(k, v)| !is_kept(k, v))
        .map(|(k, _)| invoke::id_pkg(k).to_string())
        .collect();
    let mut unmeasured: Vec<String> = docs
        .iter()
        .filter_map(|d| d["unmeasured"].as_array())
        .flatten()
        .filter_map(|p| p.as_str().map(String::from))
        .chain(packages.iter().cloned())
        .collect();
    unmeasured.retain(|p| !covered.contains(p));
    unmeasured.sort();
    unmeasured.dedup();
    covered.extend(unmeasured.iter().cloned());
    let mut items = out["items"].as_object().cloned().unwrap_or_default();
    items.retain(|k, v| is_kept(k, v) || !covered.iter().any(|p| p == invoke::id_pkg(k)));
    let mut assertions: Vec<Value> = out["assertions"].as_array().cloned().unwrap_or_default();
    assertions.retain(|a| {
        a["id"].as_str().is_none_or(|id| {
            items.contains_key(id) || !covered.iter().any(|p| p == invoke::id_pkg(id))
        })
    });
    for doc in docs {
        if let Some(shard_items) = doc["items"].as_object() {
            items.extend(shard_items.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        if let Some(shard_assertions) = doc["assertions"].as_array() {
            assertions.extend(shard_assertions.iter().cloned());
        }
        for key in ["created_unix", "noise_pct"] {
            if let Some(v) = doc[key].as_f64()
                && out[key].as_f64().is_none_or(|old| v > old)
            {
                out[key] = doc[key].clone();
            }
        }
    }
    let mut stamps = docs.iter().map(|d| &d["build"]).filter(|b| !b.is_null());
    if let Some(first) = stamps.next() {
        if stamps.any(|other| other != first) {
            return Err("the shards disagree on their build stamp".into());
        }
        out["build"] = first.clone();
    }
    out["unmeasured"] = json!(unmeasured);
    out["items"] = Value::Object(items);
    out["assertions"] = Value::Array(assertions);
    Ok(out)
}

/// Imported and `buildcost::` items belong to other runs, not to the bench
/// items a shard measures.
fn is_kept(id: &str, item: &Value) -> bool {
    invoke::is_imported(item) || id.starts_with("buildcost::")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard(items: &[&str], noise: f64, created: u64) -> Value {
        let items: serde_json::Map<String, Value> = items
            .iter()
            .map(|id| (id.to_string(), json!({ "ir": 1 })))
            .collect();
        json!({
            "version": 1, "items": items, "noise_pct": noise, "created_unix": created,
            "build": { "id": "b" }, "assertions": [{ "id": *items.keys().next().unwrap(), "ok": true }],
        })
    }

    fn ids(doc: &Value) -> Vec<String> {
        doc["items"].as_object().unwrap().keys().cloned().collect()
    }

    #[test]
    fn shards_merge_into_one_baseline_holding_every_item() {
        let merged = merge(
            &[shard(&["a::x", "a::y"], 0.5, 10), shard(&["a::z"], 2.0, 30)],
            None,
            &[],
        )
        .unwrap();
        assert_eq!(ids(&merged), ["a::x", "a::y", "a::z"]);
        assert_eq!(merged["noise_pct"], json!(2.0));
        assert_eq!(merged["created_unix"], json!(30));
        assert_eq!(merged["build"], json!({ "id": "b" }));
        assert_eq!(merged["assertions"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_merge_replaces_the_covered_packages_and_keeps_other_runs() {
        let existing = json!({
            "version": 1,
            "items": {
                "a::gone": { "ir": 9 },
                "b::other": { "ir": 2 },
                "buildcost::a::x": { "build_ms": 5 },
                "imp::old": { "imported": "tool" },
            },
            "assertions": [{ "id": "a::gone", "ok": true }, { "id": "b::other", "ok": true }],
        });
        let merged = merge(&[shard(&["a::x"], 1.0, 5)], Some(existing), &[]).unwrap();
        assert_eq!(
            ids(&merged),
            ["a::x", "b::other", "buildcost::a::x", "imp::old"]
        );
        let kept: Vec<_> = merged["assertions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_str().unwrap())
            .collect();
        assert_eq!(kept, ["b::other", "a::x"]);
    }

    #[test]
    fn a_listed_package_nobody_measured_is_noted_unmeasured_and_loses_stale_items() {
        let existing = json!({ "items": { "b::stale": { "ir": 1 }, "a::old": { "ir": 1 } } });
        let shards = [shard(&["a::x"], 1.0, 5)];
        let merged = merge(&shards, Some(existing), &["a".to_string(), "b".to_string()]).unwrap();
        assert_eq!(ids(&merged), ["a::x"]);
        assert_eq!(merged["unmeasured"], json!(["b"]));
    }

    #[test]
    fn a_shards_own_unmeasured_list_carries_into_the_merge() {
        let mut one = shard(&["a::x"], 1.0, 5);
        one["unmeasured"] = json!(["b", "a"]);
        let merged = merge(&[one], None, &[]).unwrap();
        assert_eq!(merged["unmeasured"], json!(["b"]));
    }

    #[test]
    fn shards_with_different_build_stamps_do_not_merge() {
        let mut other = shard(&["a::y"], 1.0, 5);
        other["build"] = json!({ "id": "other" });
        let err = merge(&[shard(&["a::x"], 1.0, 5), other], None, &[]).unwrap_err();
        assert_eq!(err, "the shards disagree on their build stamp");
    }

    #[test]
    fn the_shards_build_stamp_replaces_the_existing_one() {
        let existing = json!({ "items": {}, "build": { "id": "old" } });
        let merged = merge(&[shard(&["a::x"], 1.0, 5)], Some(existing), &[]).unwrap();
        assert_eq!(merged["build"], json!({ "id": "b" }));
    }

    #[test]
    fn a_missing_shard_has_its_own_failure() {
        let args: Vec<String> = [
            "merge", "--into", "head", "--expect", "3", "a.json", "b.json",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let Err(Failure::Missing(e)) = merge_args(&args) else {
            panic!("a count mismatch is the missing-shards case");
        };
        assert_eq!(e, "expected 3 shard baselines, found 2");
        let args: Vec<String> = ["merge", "--into", "head", "no-such.json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(matches!(merge_args(&args), Err(Failure::Other(_))));
    }
}
