//! `soothfast.lock`: committed fingerprints for every bound item. A bind is
//! stale when the item's current fingerprint differs from the locked one;
//! `docs accept` re-locks after the prose has been re-verified by a human.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

/// Lockfile name, resolved relative to the workspace root.
pub const LOCKFILE: &str = "soothfast.lock";

/// item path → fingerprint (hex).
pub type Binds = BTreeMap<String, String>;

/// Current lockfile format. Bumped whenever the fingerprint input changes, so
/// a lock written by an older release reports as needing one re-accept rather
/// than as prose that drifted.
pub const VERSION: u64 = 2;

/// Accepted binds, and the format version they were written under.
#[derive(Debug, Default)]
pub struct Lock {
    /// Format on disk. A lockfile that does not exist yet reports [`VERSION`].
    pub version: u64,
    /// item path → fingerprint (hex).
    pub binds: Binds,
}

/// Read `root`'s lockfile. A missing file is an empty map at the current
/// version, not an error (a fresh repo has accepted nothing yet). A file
/// without a `version` key predates the field and reads as v1.
pub fn read(root: &Path) -> Result<Lock, String> {
    let path = root.join(LOCKFILE);
    if !path.exists() {
        return Ok(Lock {
            version: VERSION,
            binds: Binds::new(),
        });
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut binds = Binds::new();
    if let Some(map) = doc["binds"].as_object() {
        for (k, v) in map {
            if let Some(fp) = v.as_str() {
                binds.insert(k.clone(), fp.to_string());
            }
        }
    }
    Ok(Lock {
        version: doc["version"].as_u64().unwrap_or(1),
        binds,
    })
}

/// Persist `binds` to `root`'s lockfile as stable, pretty-printed JSON.
pub fn write(root: &Path, binds: &Binds) -> Result<(), String> {
    let doc = json!({ "version": VERSION, "binds": binds });
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    std::fs::write(root.join(LOCKFILE), text + "\n").map_err(|e| e.to_string())
}

/// Merge freshly accepted binds over `existing`. A full-scope accept replaces
/// the map outright, dropping binds that no longer exist anywhere; a partial
/// (explicit-paths) accept must preserve out-of-scope entries.
pub fn merge(existing: &Binds, fresh: &Binds, full_scope: bool) -> Binds {
    if full_scope {
        fresh.clone()
    } else {
        let mut merged = existing.clone();
        merged.extend(fresh.iter().map(|(k, v)| (k.clone(), v.clone())));
        merged
    }
}

/// The entries of `lock` a fresh accept may merge over. Fingerprints from
/// another format are not comparable to what this build derives, so they are
/// dropped instead of being carried under a stamp claiming they are current;
/// their pages read as unlocked until their own `docs accept` runs.
pub fn comparable(lock: Lock) -> Binds {
    if lock.version == VERSION {
        lock.binds
    } else {
        Binds::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binds(pairs: &[(&str, &str)]) -> Binds {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn partial_scope_merge_preserves_out_of_scope_entries() {
        let existing = binds(&[("a::x", "1111"), ("b::y", "2222")]);
        let fresh = binds(&[("a::x", "9999")]);
        let merged = merge(&existing, &fresh, false);
        assert_eq!(merged.get("a::x").map(String::as_str), Some("9999"));
        assert_eq!(merged.get("b::y").map(String::as_str), Some("2222"));
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn entries_from_another_format_are_not_merged_over() {
        let lock = Lock {
            version: VERSION - 1,
            binds: binds(&[("a::x", "1111")]),
        };
        assert!(comparable(lock).is_empty());
    }

    #[test]
    fn entries_from_this_format_survive_a_partial_accept() {
        let lock = Lock {
            version: VERSION,
            binds: binds(&[("a::x", "1111")]),
        };
        assert_eq!(comparable(lock).len(), 1);
    }

    #[test]
    fn full_scope_merge_drops_binds_that_no_longer_exist() {
        let existing = binds(&[("a::x", "1111"), ("gone::z", "2222")]);
        let fresh = binds(&[("a::x", "9999")]);
        assert_eq!(merge(&existing, &fresh, true), fresh);
    }
}
