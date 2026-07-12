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

/// Read accepted binds from `root`'s lockfile; a missing file is an empty
/// map, not an error (a fresh repo has accepted nothing yet).
pub fn read(root: &Path) -> Result<Binds, String> {
    let path = root.join(LOCKFILE);
    if !path.exists() {
        return Ok(Binds::new());
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
    Ok(binds)
}

/// Persist `binds` to `root`'s lockfile as stable, pretty-printed JSON.
pub fn write(root: &Path, binds: &Binds) -> Result<(), String> {
    let doc = json!({ "version": 1, "binds": binds });
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
    fn full_scope_merge_drops_binds_that_no_longer_exist() {
        let existing = binds(&[("a::x", "1111"), ("gone::z", "2222")]);
        let fresh = binds(&[("a::x", "9999")]);
        assert_eq!(merge(&existing, &fresh, true), fresh);
    }
}
