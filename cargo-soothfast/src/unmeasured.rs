//! A baseline's `unmeasured` list: items the PR's own binaries contain whose
//! numbers, or some of whose metrics, the baseline does not hold, each with a
//! typed reason. `docs check` reports an unchanged claim on one "not
//! evaluated" and fails a new or edited one.

use std::collections::BTreeSet;
use std::io;
use std::path::Path;

use serde_json::{Value, json};

use crate::invoke;

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

    /// Whether a measurement could have been taken on this run. Where it
    /// could, a new or edited claim must have one; where it could not, every
    /// claim the entry covers reads not evaluated, edited or not.
    pub fn measurable_here(self) -> bool {
        !matches!(
            self,
            Reason::NotGatedOnEvent | Reason::NoParent | Reason::ClaimsSkipped
        )
    }
}

/// One `unmeasured` entry: `items` of `package` lack the `metrics` (backend
/// names, or `backend.metric`; empty for every metric) for `reason`. An entry
/// covers only the ids it names. The one exception is the typed entry of an
/// event the cargo gate does not measure on: with `except_imported` it names
/// every item outside those import prefixes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Unmeasured {
    pub package: String,
    pub reason: Reason,
    pub detail: String,
    pub items: Vec<String>,
    pub metrics: Vec<String>,
    pub except_imported: Option<Vec<String>>,
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
        if let Some(prefixes) = &self.except_imported {
            v["except_imported"] = json!(prefixes);
        }
        v
    }

    /// Whether this entry names `item`.
    pub fn names(&self, item: &str) -> bool {
        self.items.iter().any(|i| i == item)
            || self.except_imported.as_ref().is_some_and(|prefixes| {
                !prefixes.iter().any(|p| item.starts_with(&format!("{p}::")))
            })
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
            except_imported: match v.get("except_imported") {
                None => None,
                Some(p) => Some(strings(p)?),
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

/// Whether an entry is one older writers left, which matched items by name
/// prefix rather than by `owners` and so means nothing now.
fn is_legacy(v: &Value) -> bool {
    v["package"].as_str() == Some("")
        || v.get("prefixes").is_some()
        || v.get("all_items").is_some()
        || v.get("shard").is_some()
}

/// The `unmeasured` entries of a baseline document, legacy ones left out.
/// A malformed entry is an error: reading past it would turn its items'
/// claims into plain failures.
pub fn read(doc: &Value) -> Result<Vec<Unmeasured>, String> {
    let Some(entries) = doc.get("unmeasured") else {
        return Ok(Vec::new());
    };
    entries
        .as_array()
        .ok_or("`unmeasured` is not a list")?
        .iter()
        .filter(|v| !is_legacy(v))
        .map(|v| Unmeasured::from_json(v).ok_or_else(|| format!("malformed unmeasured entry {v}")))
        .collect()
}

/// The line to print when `doc` predates item ownership, which reading it
/// leaves out.
pub fn legacy_note(doc: &Value, name: &str) -> Option<String> {
    let legacy = doc["unmeasured"]
        .as_array()
        .is_some_and(|es| es.iter().any(is_legacy));
    legacy.then(|| {
        format!(
            "baseline {name:?} predates item ownership; its old unmeasured entries are ignored, so re-save it"
        )
    })
}

/// The `owners` key: each package's items, from its bench binary's listing.
pub const OWNERS_KEY: &str = "owners";

/// The items baseline `doc` says `package` owns.
pub fn owned_by(doc: &Value, package: &str) -> Vec<String> {
    doc[OWNERS_KEY][package]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect()
}

/// Record in `doc` that `package` owns `ids`.
pub fn set_owners(doc: &mut Value, package: &str, ids: &[String]) {
    if !doc[OWNERS_KEY].is_object() {
        doc[OWNERS_KEY] = json!({});
    }
    doc[OWNERS_KEY][package] = json!(ids);
}

/// Every item some package of baseline `doc` owns.
pub fn owned(doc: &Value) -> BTreeSet<String> {
    doc[OWNERS_KEY]
        .as_object()
        .into_iter()
        .flatten()
        .flat_map(|(_, ids)| ids.as_array().into_iter().flatten())
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect()
}

/// Every item a package of baseline `doc` other than `package` owns.
pub fn owned_by_others(doc: &Value, package: &str) -> BTreeSet<String> {
    doc[OWNERS_KEY]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(p, _)| p.as_str() != package)
        .flat_map(|(_, ids)| ids.as_array().into_iter().flatten())
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect()
}

/// One entry of `package` for `items`, lacking `metrics`.
pub fn entry(
    package: &str,
    reason: Reason,
    detail: &str,
    metrics: &[&str],
    items: Vec<String>,
) -> Unmeasured {
    Unmeasured {
        package: package.to_string(),
        reason,
        detail: detail.to_string(),
        items,
        metrics: metrics.iter().map(|m| m.to_string()).collect(),
        ..Unmeasured::default()
    }
}

/// Drop the numbers and assertions of `ids` from `doc`, imported items aside.
pub fn drop_items(doc: &mut Value, ids: &BTreeSet<String>) {
    if let Some(items) = doc.get_mut("items").and_then(Value::as_object_mut) {
        items.retain(|id, v| v["imported"].is_string() || !ids.contains(id));
    }
    if let Some(assertions) = doc.get_mut("assertions").and_then(Value::as_array_mut) {
        assertions.retain(|a| a["id"].as_str().is_none_or(|id| !ids.contains(id)));
    }
}

/// Record that the packages of `entries` saved no numbers. With `whole`,
/// each one's prior `owners` and the items its entry names are dropped from
/// baseline `name` under `root`, and the entry's items become its owners.
/// Without, as for a filtered gate, only the named items are dropped, and
/// the package's other owners and numbers stay. An id another package owns
/// keeps its number either way, when that package saved it.
pub fn mark_in(root: &Path, name: &str, entries: &[Unmeasured], whole: bool) -> Result<(), String> {
    let mut doc = invoke::load_baseline_in(root, name)
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| json!({ "version": 1, "items": {} }));
    let mut gone: BTreeSet<String> = BTreeSet::new();
    for e in entries {
        let others = owned_by_others(&doc, &e.package);
        let theirs = |id: &String| {
            let by = doc["items"][id]["saved_by"].as_str();
            others.contains(id) && by.is_none_or(|by| by != e.package)
        };
        let mine = |id: &String| !theirs(id);
        gone.extend(e.items.iter().filter(|id| mine(id)).cloned());
        if !whole {
            continue;
        }
        gone.extend(owned_by(&doc, &e.package).into_iter().filter(mine));
        if e.items.is_empty() {
            if let Some(owners) = doc.get_mut(OWNERS_KEY).and_then(Value::as_object_mut) {
                owners.remove(&e.package);
            }
        } else {
            set_owners(&mut doc, &e.package, &e.items);
        }
    }
    drop_items(&mut doc, &gone);
    if whole {
        let packages: Vec<&str> = entries.iter().map(|e| e.package.as_str()).collect();
        clear_entries(&mut doc, |p| packages.contains(&p));
    }
    append(&mut doc, entries).map_err(|e| e.to_string())?;
    invoke::write_baseline_in(root, name, &doc).map_err(|e| e.to_string())
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

/// Drop the entries whose package `gone` picks, and every legacy entry.
fn clear_entries(doc: &mut Value, gone: impl Fn(&str) -> bool) {
    if let Some(entries) = doc.get_mut("unmeasured").and_then(Value::as_array_mut) {
        entries.retain(|e| !is_legacy(e) && !gone(e["package"].as_str().unwrap_or("")));
    }
}

/// Drop the entries of `package` (every entry for `None`), for a save that
/// measured it.
pub fn clear(doc: &mut Value, package: Option<&str>) {
    clear_entries(doc, |p| package.is_none_or(|own| own == p));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("soothfast-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_entry_reads_back_as_written() {
        let mut ungated = entry("cargo", Reason::NotGatedOnEvent, "push", &[], Vec::new());
        ungated.except_imported = Some(ids(&["jmh"]));
        let entries = vec![
            entry(
                "core",
                Reason::ShardMissing,
                "shard 2/3",
                &[],
                ids(&["soothfast::a"]),
            ),
            entry(
                "core",
                Reason::ReusedWalltime,
                "cache",
                &["walltime"],
                ids(&["soothfast::b"]),
            ),
            ungated,
        ];
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
    fn an_entry_that_names_nothing_covers_nothing() {
        let e = entry("core", Reason::GateFailed, "failed", &[], Vec::new());
        assert!(!e.covers("soothfast::x", "alloc", "allocs"));
    }

    #[test]
    fn the_ungated_entry_names_every_item_outside_the_import_prefixes() {
        let mut e = entry("cargo", Reason::NotGatedOnEvent, "push", &[], Vec::new());
        e.except_imported = Some(ids(&["jmh", "app::bench"]));
        assert!(e.names("soothfast::parse"));
        assert!(!e.names("jmh::Parse.run"));
        assert!(!e.names("app::bench::x"));
    }

    #[test]
    fn prefix_era_entries_are_left_out_with_a_note() {
        let doc = json!({ "unmeasured": [
            { "package": "", "reason": "gate-failed", "detail": "", "items": [], "all_items": true },
            { "package": "core", "reason": "gate-failed", "detail": "", "items": [], "prefixes": ["core"] },
            { "package": "core", "reason": "gate-failed", "detail": "", "items": ["soothfast::a"] },
        ] });
        let entries = read(&doc).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].items, ["soothfast::a"]);
        assert!(legacy_note(&doc, "head").is_some_and(|n| n.contains("re-save")));
        assert!(legacy_note(&json!({ "unmeasured": [] }), "head").is_none());
    }

    #[test]
    fn a_save_clears_legacy_entries() {
        let root = scratch("unmeasured-legacy");
        let doc = json!({
            "version": 1,
            "items": {},
            "unmeasured": [{ "package": "", "reason": "gate-failed", "detail": "old",
                             "items": [], "all_items": true }],
        });
        invoke::write_baseline_in(&root, "head", &doc).unwrap();
        let fresh = entry(
            "core",
            Reason::GateFailed,
            "now",
            &[],
            ids(&["soothfast::a"]),
        );
        mark_in(&root, "head", &[fresh], true).unwrap();
        let marked = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
        assert_eq!(marked["unmeasured"].as_array().unwrap().len(), 1);
        assert!(legacy_note(&marked, "head").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_malformed_entry_is_an_error() {
        let doc = json!({ "unmeasured": [{ "package": "core", "reason": "guess", "detail": "", "items": [] }] });
        assert!(read(&doc).is_err());
    }

    #[test]
    fn an_entry_covers_only_its_metrics() {
        let wall = entry(
            "core",
            Reason::ReusedWalltime,
            "cache",
            &["walltime"],
            ids(&["s::a"]),
        );
        assert!(wall.covers("s::a", "walltime", "median_ns"));
        assert!(!wall.covers("s::a", "callgrind", "ir"));
        assert!(!wall.covers("s::b", "walltime", "median_ns"));
    }

    #[test]
    fn a_failed_package_drops_only_what_it_owns_though_ids_share_a_prefix() {
        let root = scratch("unmeasured-shared-prefix");
        let doc = json!({
            "version": 1,
            "items": {
                "soothfast::a_item": { "alloc": { "allocs": 1, "bytes": 8 } },
                "soothfast::b_item": { "alloc": { "allocs": 1, "bytes": 8 } },
                "jmh::c": { "imported": "jmh" },
            },
            "owners": { "a": ["soothfast::a_item"], "b": ["soothfast::b_item"] },
            "assertions": [{ "id": "soothfast::a_item", "kind": "alloc", "ok": true, "detail": "" }],
        });
        invoke::write_baseline_in(&root, "head", &doc).unwrap();
        let failed = entry("a", Reason::GateFailed, "failed", &[], Vec::new());
        mark_in(&root, "head", &[failed], true).unwrap();
        let marked = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
        let kept: Vec<&String> = marked["items"].as_object().unwrap().keys().collect();
        assert_eq!(kept, ["jmh::c", "soothfast::b_item"]);
        assert_eq!(marked["assertions"], json!([]));
        assert!(marked[OWNERS_KEY].get("a").is_none());
        assert_eq!(owned_by(&marked, "b"), ["soothfast::b_item"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_package_keeps_a_shared_id_s_number_only_when_another_package_saved_it() {
        for (writer, kept) in [("b", true), ("a", false), ("", true)] {
            let root = scratch(&format!("unmeasured-shared-id-{writer}"));
            let mut doc = json!({
                "version": 1,
                "items": { "util::hash": { "alloc": { "allocs": 1, "bytes": 8 } } },
                "owners": { "a": ["util::hash"], "b": ["util::hash"] },
            });
            if !writer.is_empty() {
                doc["items"]["util::hash"]["saved_by"] = json!(writer);
            }
            invoke::write_baseline_in(&root, "head", &doc).unwrap();
            let failed = entry("a", Reason::GateFailed, "failed", &[], ids(&["util::hash"]));
            mark_in(&root, "head", &[failed], true).unwrap();
            let marked = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
            assert_eq!(
                marked["items"]["util::hash"].is_object(),
                kept,
                "saved by {writer}"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}
