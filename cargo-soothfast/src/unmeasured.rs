//! A baseline's `unmeasured` list: the items of a package that the PR's own
//! binaries contain but whose measurement the baseline does not hold, each
//! with a typed reason. `docs check` reports their claims "not evaluated".

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde_json::{Value, json};

use crate::invoke;

/// Why a baseline holds no measurement of an item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reason {
    /// The bench binaries matched the merge-base, so the gate took one pass
    /// without the gating counters.
    IdenticalBinaries,
    /// HEAD's run came from the run cache rather than this gate.
    StoredRun,
    /// The shard that owns the item saved no measurement.
    ShardMissing,
    /// No leg of the gate saved a measurement of the package.
    NoMeasurement,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::IdenticalBinaries => "identical-binaries",
            Reason::StoredRun => "stored-run",
            Reason::ShardMissing => "shard-missing",
            Reason::NoMeasurement => "no-measurement",
        }
    }

    fn parse(s: &str) -> Option<Reason> {
        [
            Reason::IdenticalBinaries,
            Reason::StoredRun,
            Reason::ShardMissing,
            Reason::NoMeasurement,
        ]
        .into_iter()
        .find(|r| r.as_str() == s)
    }
}

/// One `unmeasured` entry: `items` of `package` (an id prefix, `-` written
/// `_`), with no measurement for `reason`. `items` is empty when nothing
/// named the package's items, and then no claim reads as not evaluated.
#[derive(Debug, Clone, PartialEq)]
pub struct Unmeasured {
    pub package: String,
    pub reason: Reason,
    pub detail: String,
    pub items: Vec<String>,
}

impl Unmeasured {
    pub fn to_json(&self) -> Value {
        json!({
            "package": self.package,
            "reason": self.reason.as_str(),
            "detail": self.detail,
            "items": self.items,
        })
    }

    fn from_json(v: &Value) -> Option<Unmeasured> {
        Some(Unmeasured {
            package: v["package"].as_str()?.to_string(),
            reason: Reason::parse(v["reason"].as_str()?)?,
            detail: v["detail"].as_str()?.to_string(),
            items: v["items"]
                .as_array()?
                .iter()
                .map(|id| id.as_str().map(str::to_string))
                .collect::<Option<_>>()?,
        })
    }
}

/// The `unmeasured` entries of a baseline document. A malformed entry is an
/// error: reading past it would turn its items' claims into plain failures.
pub fn read(doc: &Value) -> Result<Vec<Unmeasured>, String> {
    let Some(entries) = doc.get("unmeasured") else {
        return Ok(Vec::new());
    };
    entries
        .as_array()
        .ok_or("`unmeasured` is not a list")?
        .iter()
        .map(|v| Unmeasured::from_json(v).ok_or_else(|| format!("malformed unmeasured entry {v}")))
        .collect()
}

/// `ids` grouped into one entry per package.
pub fn by_package<'a>(
    reason: Reason,
    detail: &str,
    ids: impl IntoIterator<Item = &'a String>,
) -> Vec<Unmeasured> {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for id in ids {
        grouped
            .entry(invoke::id_pkg(id).to_string())
            .or_default()
            .push(id.clone());
    }
    grouped
        .into_iter()
        .map(|(package, items)| Unmeasured {
            package,
            reason,
            detail: detail.to_string(),
            items,
        })
        .collect()
}

/// Record `entries` in baseline `name` under `root`, dropping every item,
/// assertion and earlier entry of their packages so no older number stays.
pub fn mark_in(root: &Path, name: &str, entries: &[Unmeasured]) -> io::Result<()> {
    let mut doc = invoke::load_baseline_in(root, name)?
        .unwrap_or_else(|| json!({ "version": 1, "items": {} }));
    let of_marked = |id: &str| entries.iter().any(|e| invoke::id_pkg(id) == e.package);
    if let Some(items) = doc.get_mut("items").and_then(Value::as_object_mut) {
        items.retain(|id, v| v["imported"].is_string() || !of_marked(id));
    }
    if let Some(assertions) = doc.get_mut("assertions").and_then(Value::as_array_mut) {
        assertions.retain(|a| a["id"].as_str().is_none_or(|id| !of_marked(id)));
    }
    let mut kept: Vec<Value> = read(&doc)
        .map_err(io::Error::other)?
        .iter()
        .filter(|u| !entries.iter().any(|e| e.package == u.package))
        .map(Unmeasured::to_json)
        .collect();
    kept.extend(entries.iter().map(Unmeasured::to_json));
    doc["unmeasured"] = Value::Array(kept);
    invoke::write_baseline_in(root, name, &doc)
}

/// Drop the entries of `package` (every entry for `None`), for a save that
/// measured it.
pub fn clear(doc: &mut Value, package: Option<&str>) {
    if let Some(entries) = doc.get_mut("unmeasured").and_then(Value::as_array_mut) {
        entries.retain(|e| package.is_some_and(|p| e["package"].as_str() != Some(p)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("soothfast-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn an_entry_reads_back_as_written() {
        let ids = ["core::a".to_string(), "core::b".to_string()];
        let entries = by_package(Reason::ShardMissing, "shard 2/3", &ids);
        let doc =
            json!({ "unmeasured": entries.iter().map(Unmeasured::to_json).collect::<Vec<_>>() });
        assert_eq!(read(&doc).unwrap(), entries);
    }

    #[test]
    fn a_malformed_entry_is_an_error() {
        let doc = json!({ "unmeasured": [{ "package": "core", "reason": "guess", "detail": "", "items": [] }] });
        assert!(read(&doc).is_err());
    }

    #[test]
    fn marking_a_package_drops_its_stale_numbers_only() {
        let root = scratch("unmeasured-mark");
        let doc = json!({
            "version": 1,
            "items": {
                "core::a": { "alloc": { "allocs": 1, "bytes": 8 } },
                "other::b": { "alloc": { "allocs": 1, "bytes": 8 } },
                "jmh::c": { "imported": "jmh" },
            },
            "assertions": [{ "id": "core::a", "kind": "alloc", "ok": true, "detail": "" }],
        });
        invoke::write_baseline_in(&root, "head", &doc).unwrap();
        let ids = ["core::a".to_string()];
        mark_in(
            &root,
            "head",
            &by_package(Reason::IdenticalBinaries, "same", &ids),
        )
        .unwrap();
        let marked = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
        let kept: Vec<&String> = marked["items"].as_object().unwrap().keys().collect();
        assert_eq!(kept, ["jmh::c", "other::b"]);
        assert_eq!(marked["assertions"], json!([]));
        assert_eq!(read(&marked).unwrap()[0].items, ["core::a"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
