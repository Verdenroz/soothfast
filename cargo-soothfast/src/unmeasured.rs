//! A baseline's `unmeasured` list: items the PR's own binaries contain whose
//! numbers, or some of whose metrics, the baseline does not hold, each with a
//! typed reason. `docs check` reports an unchanged claim on one "not
//! evaluated" and fails a new or edited one.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde_json::{Value, json};

use crate::invoke::{self, Shard};

/// Why a baseline holds no number for an item's metric.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Reason {
    /// The bench binaries matched the merge-base and no stored run of that
    /// binary held the gating counters, so the gate took none.
    IdenticalBinaries,
    /// The walltime came from another run of the same code, which another
    /// host or load could have skewed.
    ReusedWalltime,
    /// The shard that owns the item saved no measurement.
    ShardMissing,
    /// The package's gate failed or errored, so it saved no numbers.
    GateFailed,
    /// A default-branch push whose commit has no parent to gate against.
    NoParent,
    /// No leg of the gate wrote anything for the package.
    #[default]
    NoMeasurement,
    /// `docs check --skip-claims` evaluated no claim.
    ClaimsSkipped,
    /// The cargo gate measures only on a pull request or a default-branch
    /// push, and this event was neither.
    NotGatedOnEvent,
    /// Read from baselines older writers left: HEAD's run came from the run
    /// cache. Never written.
    StoredRun,
}

const REASONS: [Reason; 9] = [
    Reason::IdenticalBinaries,
    Reason::ReusedWalltime,
    Reason::ShardMissing,
    Reason::GateFailed,
    Reason::NoParent,
    Reason::NoMeasurement,
    Reason::ClaimsSkipped,
    Reason::NotGatedOnEvent,
    Reason::StoredRun,
];

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::IdenticalBinaries => "identical-binaries",
            Reason::ReusedWalltime => "reused-walltime",
            Reason::ShardMissing => "shard-missing",
            Reason::GateFailed => "gate-failed",
            Reason::NoParent => "no-parent",
            Reason::NoMeasurement => "no-measurement",
            Reason::ClaimsSkipped => "claims-skipped",
            Reason::NotGatedOnEvent => "not-gated-on-event",
            Reason::StoredRun => "stored-run",
        }
    }

    pub fn parse(s: &str) -> Option<Reason> {
        REASONS.into_iter().find(|r| r.as_str() == s)
    }
}

/// One `unmeasured` entry: `items` of `package` (an id prefix, `-` written
/// `_`) lack the `metrics` (backend names, or `backend.metric`; empty for
/// every metric) for `reason`. `all_items` covers every item under the
/// `package::` prefix (every item, for an empty `package`), for a gate that
/// could not name them; `shard` narrows it to the items that shard owns.
/// Otherwise an empty `items` names nothing, and no claim reads as not
/// evaluated.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Unmeasured {
    pub package: String,
    pub reason: Reason,
    pub detail: String,
    pub items: Vec<String>,
    pub metrics: Vec<String>,
    pub all_items: bool,
    pub shard: Option<Shard>,
}

impl Unmeasured {
    pub fn to_json(&self) -> Value {
        let mut v = json!({
            "package": self.package,
            "reason": self.reason.as_str(),
            "detail": self.detail,
            "items": self.items,
        });
        if !self.metrics.is_empty() {
            v["metrics"] = json!(self.metrics);
        }
        if self.all_items {
            v["all_items"] = json!(true);
        }
        if let Some(shard) = self.shard {
            v["shard"] = json!(shard.to_string());
        }
        v
    }

    /// Whether this entry names `item` or, with `all_items`, its prefix.
    pub fn names(&self, item: &str) -> bool {
        self.items.iter().any(|i| i == item)
            || (self.all_items
                && self.shard.is_none_or(|s| s.owns(item))
                && (self.package.is_empty() || item.starts_with(&format!("{}::", self.package))))
    }

    /// Whether this entry drops the saved numbers of `id`.
    fn takes(&self, id: &str) -> bool {
        self.package.is_empty() || invoke::id_pkg(id) == self.package
    }

    fn from_json(v: &Value) -> Option<Unmeasured> {
        let strings = |v: &Value| -> Option<Vec<String>> {
            v.as_array()?
                .iter()
                .map(|s| s.as_str().map(str::to_string))
                .collect()
        };
        Some(Unmeasured {
            package: v["package"].as_str()?.to_string(),
            reason: Reason::parse(v["reason"].as_str()?)?,
            detail: v["detail"].as_str()?.to_string(),
            items: strings(&v["items"])?,
            metrics: match v.get("metrics") {
                None => Vec::new(),
                Some(m) => strings(m)?,
            },
            all_items: match v.get("all_items") {
                None => false,
                Some(b) => b.as_bool()?,
            },
            shard: match v.get("shard") {
                None => None,
                Some(s) => Some(Shard::parse(s.as_str()?).ok()?),
            },
        })
    }

    /// Whether this entry accounts for `backend.metric` of `item`.
    pub fn covers(&self, item: &str, backend: &str, metric: &str) -> bool {
        self.names(item)
            && (self.metrics.is_empty()
                || self
                    .metrics
                    .iter()
                    .any(|m| m == backend || *m == format!("{backend}.{metric}")))
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

/// `ids` grouped into one entry per package, lacking `metrics`.
pub fn by_package<'a>(
    reason: Reason,
    detail: &str,
    metrics: &[&str],
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
            metrics: metrics.iter().map(|m| m.to_string()).collect(),
            all_items: false,
            shard: None,
        })
        .collect()
}

/// Record that the packages of `entries` saved no numbers at all: every
/// item, assertion and earlier entry of theirs is dropped from baseline
/// `name` under `root`, so no older number stays.
pub fn mark_in(root: &Path, name: &str, entries: &[Unmeasured]) -> io::Result<()> {
    let mut doc = invoke::load_baseline_in(root, name)?
        .unwrap_or_else(|| json!({ "version": 1, "items": {} }));
    let packages: Vec<String> = entries.iter().map(|e| e.package.clone()).collect();
    let packages = packages.as_slice();
    let of_marked = |id: &str| entries.iter().any(|e| e.takes(id));
    if let Some(items) = doc.get_mut("items").and_then(Value::as_object_mut) {
        items.retain(|id, v| v["imported"].is_string() || !of_marked(id));
    }
    if let Some(assertions) = doc.get_mut("assertions").and_then(Value::as_array_mut) {
        assertions.retain(|a| a["id"].as_str().is_none_or(|id| !of_marked(id)));
    }
    clear_packages(&mut doc, packages);
    append(&mut doc, entries)?;
    invoke::write_baseline_in(root, name, &doc)
}

/// Add `entries` to baseline `name` under `root`, beside the numbers a save
/// just wrote.
pub fn append_in(root: &Path, name: &str, entries: &[Unmeasured]) -> io::Result<()> {
    let mut doc = invoke::load_baseline_in(root, name)?
        .unwrap_or_else(|| json!({ "version": 1, "items": {} }));
    append(&mut doc, entries)?;
    invoke::write_baseline_in(root, name, &doc)
}

fn append(doc: &mut Value, entries: &[Unmeasured]) -> io::Result<()> {
    let mut all: Vec<Value> = read(doc)
        .map_err(io::Error::other)?
        .iter()
        .map(Unmeasured::to_json)
        .collect();
    all.extend(entries.iter().map(Unmeasured::to_json));
    doc["unmeasured"] = Value::Array(all);
    Ok(())
}

fn clear_packages(doc: &mut Value, packages: &[String]) {
    if let Some(entries) = doc.get_mut("unmeasured").and_then(Value::as_array_mut) {
        entries.retain(|e| {
            !packages
                .iter()
                .any(|p| p.is_empty() || e["package"].as_str() == Some(p))
        });
    }
}

/// The packages whose cargo-measured items baseline `name` under `root`
/// holds, for a gate that could not tell which packages it covered.
pub fn packages_in(root: &Path, name: &str) -> io::Result<Vec<String>> {
    let doc = invoke::load_baseline_in(root, name)?.unwrap_or_default();
    let mut packages: Vec<String> = doc["items"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(id, v)| !v["imported"].is_string() && !id.starts_with("buildcost::"))
        .map(|(id, _)| invoke::id_pkg(id).to_string())
        .collect();
    packages.sort();
    packages.dedup();
    Ok(packages)
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
        let mut entries = by_package(Reason::ShardMissing, "shard 2/3", &[], &ids);
        entries.extend(by_package(
            Reason::ReusedWalltime,
            "cache",
            &["walltime"],
            &ids,
        ));
        let doc =
            json!({ "unmeasured": entries.iter().map(Unmeasured::to_json).collect::<Vec<_>>() });
        assert_eq!(read(&doc).unwrap(), entries);
    }

    #[test]
    fn a_legacy_stored_run_entry_still_reads() {
        let doc = json!({ "unmeasured": [{ "package": "core", "reason": "stored-run", "detail": "", "items": ["core::a"] }] });
        let entries = read(&doc).unwrap();
        assert_eq!(entries[0].reason, Reason::StoredRun);
        assert!(entries[0].covers("core::a", "callgrind", "ir"));
    }

    #[test]
    fn a_prefix_level_entry_names_every_item_under_its_prefix() {
        let entry = Unmeasured {
            package: "app::bench".into(),
            reason: Reason::GateFailed,
            all_items: true,
            ..Unmeasured::default()
        };
        let back = &read(&json!({ "unmeasured": [entry.to_json()] })).unwrap()[0];
        assert!(back.covers("app::bench::parse", "walltime", "median_ns"));
        assert!(!back.covers("app::other", "walltime", "median_ns"));
    }

    #[test]
    fn a_malformed_entry_is_an_error() {
        let doc = json!({ "unmeasured": [{ "package": "core", "reason": "guess", "detail": "", "items": [] }] });
        assert!(read(&doc).is_err());
    }

    #[test]
    fn an_entry_covers_only_its_metrics() {
        let ids = ["core::a".to_string()];
        let wall = &by_package(Reason::ReusedWalltime, "cache", &["walltime"], &ids)[0];
        assert!(wall.covers("core::a", "walltime", "median_ns"));
        assert!(!wall.covers("core::a", "callgrind", "ir"));
        assert!(!wall.covers("core::b", "walltime", "median_ns"));
        let all = &by_package(Reason::GateFailed, "failed", &[], &ids)[0];
        assert!(all.covers("core::a", "callgrind", "ir"));
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
        let entries = by_package(Reason::GateFailed, "failed", &[], &ids);
        mark_in(&root, "head", &entries).unwrap();
        let marked = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
        let kept: Vec<&String> = marked["items"].as_object().unwrap().keys().collect();
        assert_eq!(kept, ["jmh::c", "other::b"]);
        assert_eq!(marked["assertions"], json!([]));
        assert_eq!(read(&marked).unwrap()[0].items, ["core::a"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
