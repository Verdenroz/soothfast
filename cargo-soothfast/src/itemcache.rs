//! Per-item measurements kept on disk, keyed by the machine code the item
//! executed instead of the commit.
//!
//! An entry is reused for a binary only if every function it executed hashes
//! to the same code there (see `codekey`), so the counts it holds are the
//! counts that binary would produce. Entries live under the run cache's
//! conditions (`Runs::item_scope`), one file per item.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::codekey::{self, Binary};
use crate::invoke::{self, AssertionOutcome, ItemMetrics};
use crate::runcache;

/// Entries kept per item. Branches that alternate between a few code states
/// each keep theirs.
const PER_ITEM: usize = 8;

/// Item files kept across every scope before the oldest are dropped.
const KEEP: usize = 2048;

/// One measured item and the code it ran.
pub struct Entry {
    /// Executed functions as [`codekey::portable`] returns them.
    pub functions: Vec<(String, String)>,
    pub code_key: String,
    pub metrics: ItemMetrics,
    pub assertions: Vec<AssertionOutcome>,
    /// The host conditions the entry was measured under, for explaining a miss.
    pub measured_on: Value,
}

impl Entry {
    /// `None` when `bin` cannot hash the executed functions.
    pub fn measured(
        bin: &Binary,
        functions: Vec<(String, String)>,
        metrics: ItemMetrics,
        assertions: Vec<AssertionOutcome>,
        measured_on: Value,
    ) -> Option<Entry> {
        Some(Entry {
            code_key: bin.code_key(&functions)?,
            functions,
            metrics,
            assertions,
            measured_on,
        })
    }

    fn to_json(&self, id: &str) -> Value {
        let run = invoke::Run {
            items: BTreeMap::from([(id.to_string(), self.metrics.clone())]),
            ..Default::default()
        };
        json!({
            "functions": self.functions,
            "code_key": self.code_key,
            "item": invoke::run_to_items_value(&run)[id],
            "assertions": self.assertions.iter().map(|a| json!({
                "kind": a.kind, "ok": a.ok, "detail": a.detail,
            })).collect::<Vec<_>>(),
            "measured_on": self.measured_on,
        })
    }

    fn from_json(id: &str, v: &Value) -> Option<Entry> {
        let functions = v["functions"]
            .as_array()?
            .iter()
            .map(|f| Some((f[0].as_str()?.to_string(), f[1].as_str()?.to_string())))
            .collect::<Option<_>>()?;
        let mut run = invoke::run_from_items_value(&json!({ id: v["item"] }));
        let assertions = v["assertions"]
            .as_array()?
            .iter()
            .map(|a| {
                Some(AssertionOutcome {
                    id: id.to_string(),
                    kind: a["kind"].as_str()?.to_string(),
                    ok: a["ok"].as_bool()?,
                    detail: a["detail"].as_str()?.to_string(),
                })
            })
            .collect::<Option<_>>()?;
        Some(Entry {
            functions,
            code_key: v["code_key"].as_str()?.to_string(),
            metrics: run.items.remove(id)?,
            assertions,
            measured_on: v["measured_on"].clone(),
        })
    }
}

/// The per-item store of one scope.
pub struct Items {
    dir: PathBuf,
}

impl Items {
    /// The store for `scope` (see `Runs::item_scope`).
    pub fn open(scope: &str) -> Option<Items> {
        Some(Items::at(runcache::state_dir("items")?.join(scope)))
    }

    fn at(dir: PathBuf) -> Items {
        Items { dir }
    }

    fn path(&self, id: &str) -> PathBuf {
        let name = soothfast_registry::fnv1a(id.as_bytes());
        self.dir.join(format!("{name:016x}.json"))
    }

    fn read(&self, id: &str) -> Vec<Entry> {
        let Ok(text) = std::fs::read_to_string(self.path(id)) else {
            return Vec::new();
        };
        let Ok(doc) = serde_json::from_str::<Value>(&text) else {
            return Vec::new();
        };
        if doc["id"] != id {
            return Vec::new();
        }
        doc["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| Entry::from_json(id, e))
            .collect()
    }

    /// Every stored entry of `ids`.
    pub fn load(&self, ids: &[String]) -> Stored {
        Stored(ids.iter().map(|id| (id.clone(), self.read(id))).collect())
    }

    /// Keep `entry` as the newest of `id`'s, replacing one of the same code.
    /// Best effort, like the run cache: a failed write costs a measurement.
    pub fn store(&self, id: &str, entry: Entry) {
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let mut entries = self.read(id);
        entries.retain(|e| e.code_key != entry.code_key);
        entries.insert(0, entry);
        entries.truncate(PER_ITEM);
        let doc = json!({
            "id": id,
            "entries": entries.iter().map(|e| e.to_json(id)).collect::<Vec<_>>(),
        });
        let _ = std::fs::write(self.path(id), doc.to_string());
    }

    /// Drop the oldest item files past [`KEEP`], across every scope.
    pub fn prune(&self) {
        if let Some(root) = self.dir.parent() {
            prune(root, KEEP);
        }
    }
}

fn prune(root: &Path, keep: usize) {
    let Ok(scopes) = std::fs::read_dir(root) else {
        return;
    };
    let mut files = Vec::new();
    for scope in scopes.flatten() {
        let Ok(items) = std::fs::read_dir(scope.path()) else {
            continue;
        };
        files.extend(items.flatten().filter_map(|e| {
            let t = e.metadata().ok()?.modified().ok()?;
            Some((t, e.path()))
        }));
    }
    files.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    for (_, path) in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
}

/// Entries loaded for a set of items, to be matched against binaries.
pub struct Stored(BTreeMap<String, Vec<Entry>>);

impl Stored {
    /// The bench-binary functions every entry would need hashed.
    pub fn wanted(&self) -> BTreeSet<String> {
        self.0
            .values()
            .flatten()
            .flat_map(|e| &e.functions)
            .filter(|(ob, _)| ob == codekey::EXE)
            .map(|(_, sym)| sym.clone())
            .collect()
    }

    /// The newest entry of `id` whose functions hash to its code key in `bin`.
    pub fn lookup(&self, bin: &Binary, id: &str) -> Option<&Entry> {
        self.0
            .get(id)?
            .iter()
            .find(|e| bin.code_key(&e.functions).as_deref() == Some(&e.code_key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> Items {
        let dir =
            std::env::temp_dir().join(format!("soothfast-itemcache-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Items::at(dir.join("scope"))
    }

    fn entry(code_key: &str, ir: u64) -> Entry {
        Entry {
            functions: vec![(codekey::EXE.into(), "f".into())],
            code_key: code_key.into(),
            metrics: ItemMetrics {
                fingerprint: "fp".into(),
                ir: Some(ir),
                allocs: Some(1),
                bytes: Some(8),
                ..Default::default()
            },
            assertions: vec![AssertionOutcome {
                id: "p::a".into(),
                kind: "alloc".into(),
                ok: true,
                detail: "allocs 1 <= 2".into(),
            }],
            measured_on: json!({ "gating_backend": "callgrind" }),
        }
    }

    fn ids() -> Vec<String> {
        vec!["p::a".to_string()]
    }

    #[test]
    fn an_entry_round_trips_with_its_metrics_and_assertions() {
        let items = scratch("round-trip");
        items.store("p::a", entry("k1", 100));
        let stored = items.load(&ids());
        let got = &stored.0["p::a"][0];
        assert_eq!(got.code_key, "k1");
        assert_eq!(got.metrics.ir, Some(100));
        assert_eq!(got.metrics.fingerprint, "fp");
        assert_eq!(got.assertions[0].detail, "allocs 1 <= 2");
        assert_eq!(got.measured_on["gating_backend"], "callgrind");
        assert!(stored.wanted().contains("f"));
    }

    #[test]
    fn the_same_code_replaces_its_entry_and_the_newest_comes_first() {
        let items = scratch("replace");
        items.store("p::a", entry("k1", 100));
        items.store("p::a", entry("k2", 200));
        items.store("p::a", entry("k1", 300));
        let stored = items.load(&ids());
        let got: Vec<_> = stored.0["p::a"].iter().map(|e| e.metrics.ir).collect();
        assert_eq!(got, [Some(300), Some(200)]);
    }

    #[test]
    fn an_item_keeps_a_bounded_number_of_entries() {
        let items = scratch("bounded");
        for i in 0..PER_ITEM + 3 {
            items.store("p::a", entry(&format!("k{i}"), i as u64));
        }
        let stored = items.load(&ids());
        assert_eq!(stored.0["p::a"].len(), PER_ITEM);
        assert_eq!(stored.0["p::a"][0].code_key, format!("k{}", PER_ITEM + 2));
    }

    #[test]
    fn an_item_with_nothing_stored_loads_empty() {
        let items = scratch("empty");
        assert!(items.load(&ids()).0["p::a"].is_empty());
    }

    #[test]
    fn pruning_keeps_the_newest_files_only() {
        let items = scratch("prune");
        for i in 0..3 {
            items.store(&format!("p::{i}"), entry("k", 1));
        }
        prune(items.dir.parent().expect("scratch root"), 2);
        let left = std::fs::read_dir(&items.dir).expect("scope").count();
        assert_eq!(left, 2);
    }
}
