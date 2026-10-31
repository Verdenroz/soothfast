//! `cargo soothfast baseline merge --into NAME [--expect N] FILE...`: fold the
//! partial baselines a sharded gate saved, one per shard, into one baseline.

use std::path::Path;

use serde_json::{Value, json};

use crate::invoke;

pub fn run(args: &[String]) -> i32 {
    match merge_args(args) {
        Ok(path) => {
            println!("baseline saved: {}", path.display());
            0
        }
        Err(e) => {
            eprintln!("baseline: {e}");
            2
        }
    }
}

fn merge_args(args: &[String]) -> Result<std::path::PathBuf, String> {
    const USAGE: &str = "usage: baseline merge --into NAME [--expect N] FILE...";
    let (Some("merge"), rest) = (
        args.first().map(String::as_str),
        args.get(1..).unwrap_or(&[]),
    ) else {
        return Err(USAGE.into());
    };
    let (mut into, mut expect, mut files) = (None, None, Vec::new());
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--into" => into = it.next().cloned(),
            "--expect" => {
                expect = Some(
                    it.next()
                        .and_then(|n| n.parse::<usize>().ok())
                        .ok_or("--expect needs a count")?,
                )
            }
            file => files.push(file.to_string()),
        }
    }
    let into = into.ok_or(USAGE)?;
    if let Some(n) = expect
        && files.len() != n
    {
        return Err(format!(
            "expected {n} shard baselines, found {}",
            files.len()
        ));
    }
    let docs = files
        .iter()
        .map(|f| {
            let text = std::fs::read_to_string(Path::new(f)).map_err(|e| format!("{f}: {e}"))?;
            serde_json::from_str(&text).map_err(|e| format!("{f}: {e}"))
        })
        .collect::<Result<Vec<Value>, String>>()?;
    if docs.is_empty() {
        return Err(USAGE.into());
    }
    invoke::write_baseline(&into, &merge(&docs)).map_err(|e| e.to_string())
}

/// The union of the baselines' items and assertions. Shards own disjoint
/// items, so no item is in two; the scalar fields take the newest or the
/// noisiest, which is the most cautious reading of any shard.
pub fn merge(docs: &[Value]) -> Value {
    let mut out = json!({ "version": 1, "items": {}, "assertions": [] });
    for doc in docs {
        if let (Some(items), Some(out_items)) =
            (doc["items"].as_object(), out["items"].as_object_mut())
        {
            out_items.extend(items.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        if let Some(assertions) = doc["assertions"].as_array() {
            let out_assertions = out["assertions"].as_array_mut().expect("built above");
            out_assertions.extend(assertions.iter().cloned());
        }
        for key in ["created_unix", "noise_pct"] {
            if let Some(v) = doc[key].as_f64()
                && out[key].as_f64().is_none_or(|old| v > old)
            {
                out[key] = doc[key].clone();
            }
        }
        if out.get("build").is_none() && !doc["build"].is_null() {
            out["build"] = doc["build"].clone();
        }
    }
    out
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

    #[test]
    fn shards_merge_into_one_baseline_holding_every_item() {
        let merged = merge(&[shard(&["a::x", "a::y"], 0.5, 10), shard(&["a::z"], 2.0, 30)]);
        let ids: Vec<_> = merged["items"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(ids, ["a::x", "a::y", "a::z"]);
        assert_eq!(merged["noise_pct"], json!(2.0));
        assert_eq!(merged["created_unix"], json!(30));
        assert_eq!(merged["build"], json!({ "id": "b" }));
        assert_eq!(merged["assertions"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_missing_shard_is_an_error() {
        let args: Vec<String> = [
            "merge", "--into", "head", "--expect", "3", "a.json", "b.json",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let err = merge_args(&args).unwrap_err();
        assert_eq!(err, "expected 3 shard baselines, found 2");
    }
}
