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
use crate::linkmap::Extent;
use crate::runcache;

/// Entries kept per item. Branches that alternate between a few code states
/// each keep theirs.
const PER_ITEM: usize = 8;

/// Item files kept across every scope before the oldest are dropped.
const KEEP: usize = 2048;

/// One measured item and the code it ran.
#[derive(Clone)]
pub struct Entry {
    /// Executed functions as [`codekey::portable`] returns them.
    pub functions: Vec<(String, String)>,
    /// Hash of `functions` in the binary that was measured.
    pub code_key: String,
    /// The item as measured; its fingerprint names the claims below.
    pub metrics: ItemMetrics,
    /// Verdicts of the item's claims when it was measured.
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
            "assertions": self.assertions.iter().map(AssertionOutcome::verdict_json).collect::<Vec<_>>(),
            "measured_on": self.measured_on,
        })
    }

    fn from_json(id: &str, v: &Value) -> Option<Entry> {
        let functions = v["functions"]
            .as_array()?
            .iter()
            .map(invoke::pair)
            .collect::<Option<_>>()?;
        let mut run = invoke::run_from_items_value(&json!({ id: v["item"] }));
        let assertions = v["assertions"]
            .as_array()?
            .iter()
            .map(|a| AssertionOutcome::from_verdict_json(id, a))
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
        self.dir
            .join(format!("{}.json", runcache::hex_fnv(id.as_bytes())))
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

    /// Per binary of `bins`, the entry each item's code in it still hashes to;
    /// a miss is counted once per item, by the first binary's reason.
    pub fn reusable(
        &self,
        listed: &[BTreeMap<String, String>],
        bins: &[(PathBuf, Vec<Extent>)],
    ) -> (Vec<BTreeMap<String, Entry>>, Misses) {
        let ids: Vec<String> = listed[0]
            .keys()
            .filter(|id| listed.iter().all(|l| l.contains_key(*id)))
            .cloned()
            .collect();
        let none = || vec![BTreeMap::new(); bins.len()];
        let stored = self.load(&ids);
        let wanted = stored.wanted();
        if wanted.is_empty() {
            return (none(), Misses::all(Miss::Unstored, ids.len()));
        }
        let binaries: Option<Vec<Binary>> = bins
            .iter()
            .map(|(bin, inputs)| Binary::open(bin, &wanted, inputs))
            .collect();
        let Some(binaries) = binaries else {
            return (none(), Misses::all(Miss::Unreadable, ids.len()));
        };
        if let (Ok(want), [head, base]) = (std::env::var("SOOTHFAST_EXPLAIN"), binaries.as_slice())
        {
            explain(&want, &ids, &stored, head, base);
        }
        let mut hits = none();
        let mut misses = Misses::default();
        for id in &ids {
            let mut first_miss = None;
            for ((bin, l), side) in binaries.iter().zip(listed).zip(hits.iter_mut()) {
                match stored.lookup(bin, id, &l[id]) {
                    Ok(entry) => {
                        side.insert(id.clone(), entry.clone());
                    }
                    Err(miss) => {
                        first_miss.get_or_insert(miss);
                    }
                }
            }
            if let Some(miss) = first_miss {
                misses.count(miss);
            }
        }
        (hits, misses)
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
        files.extend(items.flatten().filter_map(runcache::modified));
    }
    runcache::drop_oldest(files, keep);
}

/// Fold the item files under `from` into `into`, both an `items` directory.
/// An item present in both keeps the entries of both, `from`'s first. Entries
/// are keyed on the code they executed, so the union serves any binary that
/// one of them matches.
pub fn merge(from: &Path, into: &Path) {
    let Ok(scopes) = std::fs::read_dir(from) else {
        return;
    };
    for scope in scopes.flatten() {
        let Ok(files) = std::fs::read_dir(scope.path()) else {
            continue;
        };
        let dest_dir = into.join(scope.file_name());
        if std::fs::create_dir_all(&dest_dir).is_err() {
            continue;
        }
        for file in files.flatten() {
            let dest = dest_dir.join(file.file_name());
            let Some(ours) = read_doc(&file.path()) else {
                continue;
            };
            let merged = match read_doc(&dest) {
                Some(theirs) if theirs["id"] == ours["id"] => union(ours, &theirs),
                _ => ours,
            };
            let _ = std::fs::write(dest, merged.to_string());
        }
    }
    prune(into, KEEP);
}

fn read_doc(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn union(mut first: Value, second: &Value) -> Value {
    let mut entries = first["entries"].as_array().cloned().unwrap_or_default();
    for entry in second["entries"].as_array().into_iter().flatten() {
        if !entries.iter().any(|e| e["code_key"] == entry["code_key"]) {
            entries.push(entry.clone());
        }
    }
    entries.truncate(PER_ITEM);
    first["entries"] = Value::Array(entries);
    first
}

/// Entries loaded for a set of items, to be matched against binaries.
pub struct Stored(BTreeMap<String, Vec<Entry>>);

impl Stored {
    /// The bench-binary functions every entry would need hashed.
    pub fn wanted(&self) -> BTreeSet<String> {
        codekey::exe_symbols(self.0.values().flatten().flat_map(|e| &e.functions))
    }

    /// The newest entry of `id` whose functions hash to its code key in `bin`
    /// and whose item fingerprint is `fingerprint`. The fingerprint carries
    /// the claims (`alloc = N`, `p99 = ...`) its stored assertions were
    /// judged against, which the executed code does not.
    pub fn lookup(&self, bin: &Binary, id: &str, fingerprint: &str) -> Result<&Entry, Miss> {
        let entries = self
            .0
            .get(id)
            .filter(|e| !e.is_empty())
            .ok_or(Miss::Unstored)?;
        let mut miss = Miss::Claims;
        for e in entries
            .iter()
            .filter(|e| e.metrics.fingerprint == fingerprint)
        {
            match bin.code_key(&e.functions) {
                Some(key) if key == e.code_key => return Ok(e),
                Some(_) => miss = Miss::Code,
                None if miss == Miss::Claims => miss = Miss::Unhashed,
                None => {}
            }
        }
        Err(miss)
    }
}

/// Print what differs between `head` and `base` for item `want`, or every
/// item for `all`, over its newest entry's functions.
fn explain(want: &str, ids: &[String], stored: &Stored, head: &Binary, base: &Binary) {
    for id in ids.iter().filter(|id| want == "all" || *id == want) {
        let Some(entry) = stored.0.get(id).and_then(|e| e.first()) else {
            continue;
        };
        let lines = head.differences(base, &entry.functions);
        if lines.is_empty() {
            continue;
        }
        println!("explain {id}: head vs base, over the newest entry's functions");
        for line in lines {
            println!("explain   {line}");
        }
    }
}

/// Why no stored entry of an item stands for it in one binary, in the order
/// the gate line reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Miss {
    /// The code an entry ran hashes differently in this binary.
    Code,
    /// No entry was measured under the item's current claims.
    Claims,
    /// A function an entry ran cannot be hashed in this binary.
    Unhashed,
    /// Nothing is stored for the item in this scope.
    Unstored,
    /// The binary could not be read at all.
    Unreadable,
}

impl Miss {
    fn reason(self) -> &'static str {
        match self {
            Miss::Code => "code changed",
            Miss::Claims => "claims changed",
            Miss::Unhashed => "unhashable here",
            Miss::Unstored => "never stored",
            Miss::Unreadable => "binary unreadable",
        }
    }
}

/// The items [`Items::reusable`] found no entry for, by reason.
#[derive(Debug, Default, PartialEq)]
pub struct Misses(BTreeMap<Miss, usize>);

impl Misses {
    fn all(miss: Miss, n: usize) -> Misses {
        Misses(BTreeMap::from([(miss, n)]))
    }

    fn count(&mut self, miss: Miss) {
        *self.0.entry(miss).or_default() += 1;
    }
}

impl std::fmt::Display for Misses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let named: Vec<String> = self
            .0
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(miss, n)| format!("{n} {}", miss.reason()))
            .collect();
        f.write_str(&named.join(", "))
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
    fn a_changed_claim_misses_though_the_code_is_the_same() {
        let binary = Binary::with_hashes([("f".to_string(), 1)].into());
        let functions = vec![(codekey::EXE.to_string(), "f".to_string())];
        let items = scratch("fingerprint");
        let stored = Entry::measured(
            &binary,
            functions,
            entry("", 1).metrics,
            Vec::new(),
            Value::Null,
        )
        .expect("hashable");
        items.store("p::a", stored);
        let loaded = items.load(&ids());
        assert!(loaded.lookup(&binary, "p::a", "fp").is_ok());
        assert_eq!(
            loaded.lookup(&binary, "p::a", "other").err(),
            Some(Miss::Claims)
        );
    }

    #[test]
    fn a_miss_names_why_no_entry_stands_for_the_item() {
        let items = scratch("why");
        items.store("p::a", entry("k1", 1));
        let loaded = items.load(&ids());
        let changed = Binary::with_hashes([("f".to_string(), 7)].into());
        let unhashed = Binary::with_hashes(Default::default());
        assert_eq!(
            loaded.lookup(&changed, "p::a", "fp").err(),
            Some(Miss::Code)
        );
        assert_eq!(
            loaded.lookup(&unhashed, "p::a", "fp").err(),
            Some(Miss::Unhashed)
        );
        let nothing = scratch("why-empty").load(&ids());
        assert_eq!(
            nothing.lookup(&changed, "p::a", "fp").err(),
            Some(Miss::Unstored)
        );
    }

    #[test]
    fn misses_print_only_the_reasons_that_occurred() {
        let mut misses = Misses::all(Miss::Unstored, 1);
        for _ in 0..3 {
            misses.count(Miss::Code);
        }
        assert_eq!(misses.to_string(), "3 code changed, 1 never stored");
    }

    #[test]
    fn an_item_with_nothing_stored_loads_empty() {
        let items = scratch("empty");
        assert!(items.load(&ids()).0["p::a"].is_empty());
    }

    #[test]
    fn items_saved_by_one_push_are_found_by_the_next() {
        let branch = scratch("merge-branch");
        let base = scratch("merge-base");
        branch.store("p::a", entry("pushed", 1));
        branch.store("p::a", entry("shared", 2));
        base.store("p::a", entry("shared", 3));
        base.store("p::a", entry("master", 4));
        branch.store("p::b", entry("only-branch", 5));
        merge(branch.dir.parent().unwrap(), base.dir.parent().unwrap());
        let both = [ids()[0].clone(), "p::b".to_string()];
        let stored = base.load(&both);
        let keys =
            |id: &str| -> Vec<String> { stored.0[id].iter().map(|e| e.code_key.clone()).collect() };
        assert_eq!(keys("p::a"), ["shared", "pushed", "master"]);
        assert_eq!(stored.0["p::a"][0].metrics.ir, Some(2));
        assert_eq!(keys("p::b"), ["only-branch"]);
    }

    #[test]
    fn a_merged_item_keeps_a_bounded_number_of_entries() {
        let branch = scratch("merge-bounded-branch");
        let base = scratch("merge-bounded-base");
        for i in 0..PER_ITEM {
            branch.store("p::a", entry(&format!("b{i}"), i as u64));
            base.store("p::a", entry(&format!("m{i}"), i as u64));
        }
        merge(branch.dir.parent().unwrap(), base.dir.parent().unwrap());
        assert_eq!(base.load(&ids()).0["p::a"].len(), PER_ITEM);
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
