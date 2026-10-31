//! Which of HEAD's dependency units a seed can copy without mixing two builds.
//!
//! A unit's name hashes what selects its outputs, not everything rustc folds
//! into the crate: two trees can hold builds of one name whose crate hashes
//! differ. Each rlib records the crate hash of every dependency it was
//! compiled against, so a HEAD unit copied beside the merge-base's own build
//! of one of its dependencies is rejected by rustc. Cargo compares only
//! fingerprints, finds both fresh, and rebuilds neither, which leaves the
//! merge-base build unable to find the crate at all.
//!
//! The graph comes from cargo's fingerprints: the hash file of a unit names
//! it, and its JSON lists each dependency by that same hash.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde_json::Value;

/// One unit's fingerprint: its own hashes and those of its dependencies.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Unit {
    own: Vec<u64>,
    deps: Vec<u64>,
}

/// Read the fingerprint directory of one unit. Files cargo has not written
/// yet read as nothing, which only makes the unit look unconnected.
pub fn read(dir: &Path) -> Unit {
    let mut unit = Unit::default();
    let Ok(entries) = fs::read_dir(dir) else {
        return unit;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        if name.ends_with(".json") {
            unit.deps.extend(dependency_hashes(&text));
        } else if !name.starts_with("dep-")
            && !name.starts_with("output-")
            && name != "invoked.timestamp"
        {
            unit.own.extend(own_hash(&text));
        }
    }
    unit
}

/// Cargo writes a fingerprint hash as the hex of its little-endian bytes.
fn own_hash(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.len() != 16 {
        return None;
    }
    u64::from_str_radix(text, 16).ok().map(u64::swap_bytes)
}

/// Each `deps` entry is `[package, crate name, public, fingerprint hash]`.
fn dependency_hashes(text: &str) -> Vec<u64> {
    let Ok(json) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    json["deps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d[3].as_u64())
        .collect()
}

/// The `candidates` (units of `head` that `base` lacks) a seed must leave for
/// the base build, each with why: it depends, directly or not, on a unit
/// `base` built itself, or a unit of `base` was built against it.
pub fn blocked(
    head: &BTreeMap<String, Unit>,
    base: &BTreeMap<String, Unit>,
    candidates: &[String],
) -> BTreeMap<String, String> {
    let by_hash: BTreeMap<u64, &str> = head
        .iter()
        .flat_map(|(name, unit)| unit.own.iter().map(move |h| (*h, name.as_str())))
        .collect();
    let base_needs: BTreeSet<u64> = base.values().flat_map(|u| u.deps.iter().copied()).collect();
    let graph = Graph {
        head,
        base,
        by_hash,
        base_needs,
    };
    let mut memo = BTreeMap::new();
    candidates
        .iter()
        .filter_map(|name| {
            graph
                .conflict(name, &mut memo)
                .map(|why| (name.clone(), why))
        })
        .collect()
}

struct Graph<'a> {
    head: &'a BTreeMap<String, Unit>,
    base: &'a BTreeMap<String, Unit>,
    by_hash: BTreeMap<u64, &'a str>,
    base_needs: BTreeSet<u64>,
}

impl Graph<'_> {
    fn conflict(&self, name: &str, memo: &mut BTreeMap<String, Option<String>>) -> Option<String> {
        if let Some(known) = memo.get(name) {
            return known.clone();
        }
        let found = self.find(name, memo);
        memo.insert(name.to_string(), found.clone());
        found
    }

    fn find(&self, name: &str, memo: &mut BTreeMap<String, Option<String>>) -> Option<String> {
        let unit = self.head.get(name)?;
        for dep in unit.deps.iter().filter_map(|h| self.by_hash.get(h)) {
            if self.base.contains_key(*dep) {
                return Some(format!(
                    "depends on {dep}, which the merge-base built itself"
                ));
            }
            if let Some(why) = self.conflict(dep, memo) {
                return Some(why);
            }
        }
        unit.own
            .iter()
            .any(|h| self.base_needs.contains(h))
            .then(|| format!("the merge-base built its own dependents of {name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(own: u64, deps: &[u64]) -> Unit {
        Unit {
            own: vec![own],
            deps: deps.to_vec(),
        }
    }

    #[test]
    fn a_fingerprint_hash_reads_as_cargo_writes_it() {
        assert_eq!(own_hash("8748516213b072d9\n"), Some(15668779650875738247));
        assert_eq!(own_hash("nothex"), None);
        let json = r#"{"deps":[[13973934561794393430,"linkme",false,15668779650875738247]]}"#;
        assert_eq!(dependency_hashes(json), [15668779650875738247]);
    }

    #[test]
    fn a_unit_built_on_the_merge_bases_own_dependency_is_blocked_with_its_dependents() {
        let head = BTreeMap::from([
            ("linkme-a".to_string(), unit(1, &[])),
            ("registry-b".to_string(), unit(2, &[1])),
            ("facade-c".to_string(), unit(3, &[2])),
            ("itoa-d".to_string(), unit(4, &[])),
        ]);
        let base = BTreeMap::from([("linkme-a".to_string(), unit(1, &[]))]);
        let candidates = ["registry-b", "facade-c", "itoa-d"].map(String::from);
        let blocked = blocked(&head, &base, &candidates);
        assert_eq!(
            blocked.keys().collect::<Vec<_>>(),
            ["facade-c", "registry-b"]
        );
        assert!(blocked["facade-c"].contains("linkme-a"), "{blocked:?}");
    }

    #[test]
    fn a_unit_the_merge_base_was_built_against_is_left_to_it() {
        let head = BTreeMap::from([("libc-a".to_string(), unit(1, &[]))]);
        let base = BTreeMap::from([("measure-b".to_string(), unit(2, &[1]))]);
        let blocked = blocked(&head, &base, &["libc-a".to_string()]);
        assert!(blocked.contains_key("libc-a"), "{blocked:?}");
    }

    #[test]
    fn a_cold_merge_base_blocks_nothing() {
        let head = BTreeMap::from([
            ("linkme-a".to_string(), unit(1, &[])),
            ("registry-b".to_string(), unit(2, &[1])),
        ]);
        let candidates = ["linkme-a", "registry-b"].map(String::from);
        assert!(blocked(&head, &BTreeMap::new(), &candidates).is_empty());
    }
}
