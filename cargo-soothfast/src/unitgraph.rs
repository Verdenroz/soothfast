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
//! it, and the JSON of a compile lists each dependency by that same hash. Only
//! the edges rustc checks a crate hash on are kept: a compile's library and
//! proc-macro dependencies. A build script's own compile and its run link
//! nothing a later compile reads by crate hash.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde_json::Value;

/// One unit's fingerprint: its own hashes and the crate-hash-checked
/// dependencies of its compile.
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
        let kind = if let Some(stem) = name.strip_suffix(".json") {
            if stem.starts_with("build-script-") || stem.starts_with("run-build-script-") {
                continue;
            }
            Kind::Compile
        } else if name.starts_with("dep-")
            || name.starts_with("output-")
            || name == "invoked.timestamp"
        {
            continue;
        } else {
            Kind::Hash
        };
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        match kind {
            Kind::Compile => unit.deps.extend(checked_dependencies(&text)),
            Kind::Hash => unit.own.extend(own_hash(&text)),
        }
    }
    unit
}

enum Kind {
    Compile,
    Hash,
}

/// Cargo writes a fingerprint hash as the hex of its little-endian bytes.
fn own_hash(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.len() != 16 {
        return None;
    }
    u64::from_str_radix(text, 16).ok().map(u64::swap_bytes)
}

/// Each `deps` entry is `[package, crate name, public, fingerprint hash]`. The
/// edge to the package's build-script run is named after the script target.
fn checked_dependencies(text: &str) -> Vec<u64> {
    let Ok(json) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    json["deps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| {
            !d[1]
                .as_str()
                .is_some_and(|n| n.starts_with("build_script_"))
        })
        .filter_map(|d| d[3].as_u64())
        .collect()
}

/// The `candidates` (units of `head` that `base` lacks) a seed must leave for
/// the base build, each with why. `copies` names the units of `base` whose
/// outputs are HEAD's own, as a seed copied them; every other unit of `base`
/// is the merge-base's own build.
pub fn blocked(
    head: &BTreeMap<String, Unit>,
    base: &BTreeMap<String, Unit>,
    copies: &BTreeSet<String>,
    candidates: &[String],
) -> BTreeMap<String, String> {
    let own: BTreeSet<&str> = base
        .iter()
        .filter(|(name, _)| !copies.contains(*name))
        .map(|(name, _)| name.as_str())
        .collect();
    let graph = Graph {
        head,
        base,
        own_needs: base
            .iter()
            .filter(|(name, _)| own.contains(name.as_str()))
            .flat_map(|(_, u)| u.deps.iter().copied())
            .collect(),
        own,
        head_by_hash: by_hash(head),
        base_by_hash: by_hash(base),
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

/// The units of `base` a candidate depends on or that depend on one: the only
/// ones whose provenance [`blocked`] reads.
pub fn touching(
    head: &BTreeMap<String, Unit>,
    base: &BTreeMap<String, Unit>,
    candidates: &[String],
) -> BTreeSet<String> {
    let (head_by_hash, base_by_hash) = (by_hash(head), by_hash(base));
    let units: Vec<&Unit> = candidates.iter().filter_map(|c| head.get(c)).collect();
    let own: BTreeSet<u64> = units.iter().flat_map(|u| u.own.iter().copied()).collect();
    let mut out: BTreeSet<String> = units
        .iter()
        .flat_map(|u| u.deps.iter())
        .filter_map(|h| {
            head_by_hash
                .get(h)
                .filter(|name| base.contains_key(**name))
                .or_else(|| base_by_hash.get(h))
                .map(|name| name.to_string())
        })
        .collect();
    out.extend(
        base.iter()
            .filter(|(_, u)| u.deps.iter().any(|h| own.contains(h)))
            .map(|(name, _)| name.clone()),
    );
    out
}

fn by_hash(units: &BTreeMap<String, Unit>) -> BTreeMap<u64, &str> {
    units
        .iter()
        .flat_map(|(name, unit)| unit.own.iter().map(move |h| (*h, name.as_str())))
        .collect()
}

struct Graph<'a> {
    head: &'a BTreeMap<String, Unit>,
    base: &'a BTreeMap<String, Unit>,
    /// Units of `base` it built itself rather than took from a seed.
    own: BTreeSet<&'a str>,
    /// Every hash those units were compiled against.
    own_needs: BTreeSet<u64>,
    head_by_hash: BTreeMap<u64, &'a str>,
    base_by_hash: BTreeMap<u64, &'a str>,
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
        for hash in &unit.deps {
            if let Some(why) = self.dependency(*hash, memo) {
                return Some(why);
            }
        }
        unit.own
            .iter()
            .any(|h| self.own_needs.contains(h))
            .then(|| format!("the merge-base built its own dependents of {name}"))
    }

    fn dependency(&self, hash: u64, memo: &mut BTreeMap<String, Option<String>>) -> Option<String> {
        let built_by_base = |dep: &str| {
            self.own
                .contains(dep)
                .then(|| format!("depends on {dep}, which the merge-base built itself"))
        };
        match (self.head_by_hash.get(&hash), self.base_by_hash.get(&hash)) {
            (Some(dep), _) if self.base.contains_key(*dep) => built_by_base(dep),
            (Some(dep), _) => self.conflict(dep, memo),
            (None, Some(dep)) => built_by_base(dep),
            (None, None) => Some(format!(
                "depends on fingerprint {hash:016x}, which neither target dir holds"
            )),
        }
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

    fn units(list: &[(&str, Unit)]) -> BTreeMap<String, Unit> {
        list.iter()
            .map(|(n, u)| (n.to_string(), u.clone()))
            .collect()
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_fingerprint_hash_reads_as_cargo_writes_it() {
        assert_eq!(own_hash("8748516213b072d9\n"), Some(15668779650875738247));
        assert_eq!(own_hash("nothex"), None);
        let json = r#"{"deps":[[1,"linkme",false,15668779650875738247],[2,"build_script_build",false,7]]}"#;
        assert_eq!(checked_dependencies(json), [15668779650875738247]);
    }

    #[test]
    fn a_unit_built_on_the_merge_bases_own_dependency_is_blocked_with_its_dependents() {
        let head = units(&[
            ("linkme-a", unit(1, &[])),
            ("registry-b", unit(2, &[1])),
            ("facade-c", unit(3, &[2])),
            ("itoa-d", unit(4, &[])),
        ]);
        let base = units(&[("linkme-a", unit(1, &[]))]);
        let candidates = names(&["registry-b", "facade-c", "itoa-d"]);
        let blocked = blocked(&head, &base, &BTreeSet::new(), &candidates);
        assert_eq!(
            blocked.keys().collect::<Vec<_>>(),
            ["facade-c", "registry-b"]
        );
        assert!(blocked["facade-c"].contains("linkme-a"), "{blocked:?}");
    }

    #[test]
    fn a_dependency_an_earlier_seed_copied_does_not_block() {
        let head = units(&[("linkme-a", unit(1, &[])), ("registry-b", unit(2, &[1]))]);
        let base = units(&[("linkme-a", unit(1, &[]))]);
        let copies = BTreeSet::from(["linkme-a".to_string()]);
        let candidates = names(&["registry-b"]);
        assert!(blocked(&head, &base, &copies, &candidates).is_empty());
        assert!(blocked(&head, &base, &BTreeSet::new(), &candidates).contains_key("registry-b"));
    }

    #[test]
    fn a_dependency_head_rebuilt_in_place_is_resolved_or_blocks() {
        let head = units(&[("d-a", unit(5, &[])), ("c-b", unit(2, &[1]))]);
        let candidates = names(&["c-b"]);
        let base_built = units(&[("d-a", unit(1, &[]))]);
        let why = blocked(&head, &base_built, &BTreeSet::new(), &candidates);
        assert!(why["c-b"].contains("d-a"), "{why:?}");
        let why = blocked(&head, &BTreeMap::new(), &BTreeSet::new(), &candidates);
        assert!(why["c-b"].contains("neither target dir"), "{why:?}");
    }

    #[test]
    fn a_unit_the_merge_base_was_built_against_is_left_to_it() {
        let head = units(&[("libc-a", unit(1, &[]))]);
        let base = units(&[("measure-b", unit(2, &[1]))]);
        let candidates = names(&["libc-a"]);
        assert!(blocked(&head, &base, &BTreeSet::new(), &candidates).contains_key("libc-a"));
        let copies = BTreeSet::from(["measure-b".to_string()]);
        assert!(blocked(&head, &base, &copies, &candidates).is_empty());
    }

    #[test]
    fn only_units_next_to_a_candidate_are_touched() {
        let head = units(&[
            ("linkme-a", unit(1, &[])),
            ("registry-b", unit(2, &[1])),
            ("itoa-d", unit(4, &[])),
        ]);
        let base = units(&[
            ("linkme-a", unit(1, &[])),
            ("itoa-d", unit(4, &[])),
            ("measure-e", unit(5, &[2])),
        ]);
        let touched = touching(&head, &base, &names(&["registry-b"]));
        assert_eq!(
            touched.into_iter().collect::<Vec<_>>(),
            ["linkme-a", "measure-e"]
        );
    }

    #[test]
    fn a_cold_merge_base_blocks_nothing() {
        let head = units(&[("linkme-a", unit(1, &[])), ("registry-b", unit(2, &[1]))]);
        let candidates = names(&["linkme-a", "registry-b"]);
        assert!(blocked(&head, &BTreeMap::new(), &BTreeSet::new(), &candidates).is_empty());
    }

    #[test]
    fn a_build_script_edge_is_not_followed() {
        let dir = std::env::temp_dir().join(format!("soothfast-unitgraph-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("lib-x"), "0100000000000000").unwrap();
        fs::write(
            dir.join("lib-x.json"),
            r#"{"deps":[[1,"y",false,2],[1,"build_script_build",false,3]]}"#,
        )
        .unwrap();
        fs::write(
            dir.join("run-build-script-build-script-build.json"),
            r#"{"deps":[[1,"z",false,4]]}"#,
        )
        .unwrap();
        fs::write(dir.join("dep-lib-x"), "binary").unwrap();
        let read = read(&dir);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(read, unit(1, &[2]));
    }
}
