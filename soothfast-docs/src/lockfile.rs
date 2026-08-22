//! `soothfast.lock`: committed fingerprints for every bound item. A bind is
//! stale when the item's current fingerprint differs from the locked one;
//! `docs accept` re-locks after the prose has been re-verified by a human.
//!
//! Exported items are locked the same way, and as a set: adding or dropping
//! one changes the key map, so a widened binding surface is as visible as a
//! changed one.

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

/// Every fingerprint family the lockfile carries, and the format version they
/// were written under.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lock {
    /// Format on disk. A lockfile that does not exist yet reports [`VERSION`].
    pub version: u64,
    /// Items `<!-- soothfast:bind -->` prose is pinned to.
    pub binds: Binds,
    /// Items `#[soothfast::export]` binds into other languages.
    pub exports: Binds,
}

/// Read the lockfile at `root`. A missing file is empty at the current
/// version, not an error (a fresh repo has accepted nothing yet). A file
/// without a `version` key predates the field and reads as v1.
pub fn read(root: &Path) -> Result<Lock, String> {
    let path = root.join(LOCKFILE);
    if !path.exists() {
        return Ok(Lock {
            version: VERSION,
            ..Lock::default()
        });
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(Lock {
        version: doc["version"].as_u64().unwrap_or(1),
        binds: family(&doc, "binds"),
        exports: family(&doc, "exports"),
    })
}

fn family(doc: &Value, key: &str) -> Binds {
    let mut out = Binds::new();
    if let Some(map) = doc[key].as_object() {
        for (k, v) in map {
            if let Some(fp) = v.as_str() {
                out.insert(k.clone(), fp.to_string());
            }
        }
    }
    out
}

/// Persist `lock` to `root`'s lockfile as stable, pretty-printed JSON.
///
/// A family with nothing in it is left out, so a repo that binds no
/// languages keeps the file it always had.
pub fn write(root: &Path, lock: &Lock) -> Result<(), String> {
    let mut doc = json!({ "version": VERSION, "binds": lock.binds });
    if !lock.exports.is_empty() {
        doc["exports"] = json!(lock.exports);
    }
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

/// The entries of `lock` a fresh accept may merge over.
///
/// Fingerprints from an older format are not comparable to what this build
/// derives, so they are dropped rather than carried under a stamp claiming
/// they are current; their pages read as unlocked until their own
/// `docs accept` runs, which is what lets a repo migrate one accept at a
/// time.
///
/// A lock from a *newer* format is equally incomparable but is not this
/// build's to discard, so it is refused instead. The asymmetry is the point:
/// an older entry is worthless, a newer one is someone else's valid work.
/// Accepting is deliberately stricter than checking here: a check against a
/// newer lock only misreports, while an accept would overwrite it.
pub fn comparable(lock: Lock) -> Result<Binds, String> {
    if lock.version > VERSION {
        return Err(format!(
            "{LOCKFILE} is format v{}, this build writes v{VERSION}: upgrade cargo-soothfast rather than overwriting a newer lock",
            lock.version
        ));
    }
    if lock.version == VERSION {
        Ok(lock.binds)
    } else {
        Ok(Binds::new())
    }
}

/// Replace one package's slice of a family, leaving every other package's
/// alone.
///
/// The lockfile spans the workspace but an accept runs per package, and item
/// ids start with the crate that declared them.
pub fn merge_scoped(existing: &Binds, fresh: &Binds, prefix: &str) -> Binds {
    let mut merged: Binds = existing
        .iter()
        .filter(|(k, _)| !k.starts_with(prefix))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    merged.extend(fresh.iter().map(|(k, v)| (k.clone(), v.clone())));
    merged
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
    fn entries_from_an_older_format_are_not_merged_over() {
        let lock = Lock {
            version: VERSION - 1,
            binds: binds(&[("a::x", "1111")]),
            exports: Binds::new(),
        };
        assert!(comparable(lock).unwrap().is_empty());
    }

    #[test]
    fn entries_from_this_format_survive_a_partial_accept() {
        let lock = Lock {
            version: VERSION,
            binds: binds(&[("a::x", "1111")]),
            exports: Binds::new(),
        };
        assert_eq!(comparable(lock).unwrap().len(), 1);
    }

    #[test]
    fn a_newer_lock_is_refused_rather_than_overwritten() {
        let lock = Lock {
            version: VERSION + 1,
            binds: binds(&[("a::x", "1111")]),
            exports: Binds::new(),
        };
        assert!(comparable(lock).is_err());
    }

    #[test]
    fn full_scope_merge_drops_binds_that_no_longer_exist() {
        let existing = binds(&[("a::x", "1111"), ("gone::z", "2222")]);
        let fresh = binds(&[("a::x", "9999")]);
        assert_eq!(merge(&existing, &fresh, true), fresh);
    }

    #[test]
    fn a_scoped_merge_replaces_one_package_and_leaves_the_rest() {
        let existing = binds(&[("a::x", "1111"), ("a::gone", "2222"), ("b::y", "3333")]);
        let fresh = binds(&[("a::x", "9999"), ("a::new", "4444")]);
        let merged = merge_scoped(&existing, &fresh, "a::");
        assert_eq!(merged.get("a::x").map(String::as_str), Some("9999"));
        assert_eq!(merged.get("a::new").map(String::as_str), Some("4444"));
        assert_eq!(merged.get("b::y").map(String::as_str), Some("3333"));
        assert!(!merged.contains_key("a::gone"));
    }

    #[test]
    fn an_empty_exports_family_never_reaches_the_file() {
        let dir = std::env::temp_dir().join(format!("soothfast-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("makes dir");
        let lock = Lock {
            version: VERSION,
            binds: binds(&[("a::x", "1111")]),
            exports: Binds::new(),
        };
        write(&dir, &lock).expect("writes");
        let text = std::fs::read_to_string(dir.join(LOCKFILE)).expect("reads");
        assert!(!text.contains("exports"));
        assert_eq!(read(&dir).expect("reads"), lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn both_families_survive_a_round_trip() {
        let dir = std::env::temp_dir().join(format!("soothfast-lock2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("makes dir");
        let lock = Lock {
            version: VERSION,
            binds: binds(&[("a::x", "1111")]),
            exports: binds(&[("a::Counter", "2222")]),
        };
        write(&dir, &lock).expect("writes");
        assert_eq!(read(&dir).expect("reads"), lock);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
