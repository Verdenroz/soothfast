//! `soothfast-gate.lock`: committed, reviewed overrides for one-time
//! regressions from bug fixes. `gate accept` records a specific bench's
//! failing metrics at their new value; `gate` consumes an entry as long as
//! `base_fingerprint` still matches the reference side, treating the
//! accepted value (not the original reference value) as the new ceiling.
//! `head_fingerprint` is unrelated to that check — it only tells a baseline
//! save when the accepted shift has become the norm and the entry can be
//! dropped (see `prune_absorbed`).

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::invoke::Run;

/// Lockfile name, resolved relative to the workspace root. A top-level file
/// (like `soothfast.lock`, `Cargo.lock`), not under `.soothfast/` — that
/// directory is gitignored wholesale, and a committed audit trail placed
/// inside it would never reach a PR diff.
pub const LOCKFILE: &str = "soothfast-gate.lock";

/// One reviewed reason an entry's metrics were accepted or re-accepted.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Justification {
    pub text: String,
    pub accepted_unix: u64,
}

/// An accepted bench: the fingerprints the accept was reviewed against, the
/// metrics it covers, and every justification a re-accept has appended.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Entry {
    pub base_fingerprint: String,
    pub head_fingerprint: String,
    pub metrics: BTreeMap<String, f64>,
    pub justifications: Vec<Justification>,
}

/// Accepted benches, keyed by `full_id`.
pub type Entries = BTreeMap<String, Entry>;

/// Read accepted entries from `root`'s lockfile; a missing file is an empty
/// map, not an error (a fresh repo has accepted nothing yet).
pub fn read(root: &Path) -> Result<Entries, String> {
    let path = root.join(LOCKFILE);
    if !path.exists() {
        return Ok(Entries::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut entries = Entries::new();
    let Some(map) = doc["entries"].as_object() else {
        return Ok(entries);
    };
    for (id, v) in map {
        let metrics = v["metrics"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.clone(), v.as_f64()?)))
            .collect();
        let justifications = v["justifications"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|j| {
                Some(Justification {
                    text: j["text"].as_str()?.to_string(),
                    accepted_unix: j["accepted_unix"].as_u64().unwrap_or(0),
                })
            })
            .collect();
        entries.insert(
            id.clone(),
            Entry {
                base_fingerprint: v["base_fingerprint"].as_str().unwrap_or("").to_string(),
                head_fingerprint: v["head_fingerprint"].as_str().unwrap_or("").to_string(),
                metrics,
                justifications,
            },
        );
    }
    Ok(entries)
}

/// Persist `entries` to `root`'s lockfile as stable, pretty-printed JSON.
pub fn write(root: &Path, entries: &Entries) -> Result<(), String> {
    let mut map = serde_json::Map::new();
    for (id, e) in entries {
        map.insert(
            id.clone(),
            json!({
                "base_fingerprint": e.base_fingerprint,
                "head_fingerprint": e.head_fingerprint,
                "metrics": e.metrics,
                "justifications": e.justifications.iter().map(|j| json!({
                    "text": j.text,
                    "accepted_unix": j.accepted_unix,
                })).collect::<Vec<_>>(),
            }),
        );
    }
    let doc = json!({ "version": 1, "entries": map });
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    std::fs::write(root.join(LOCKFILE), text + "\n").map_err(|e| e.to_string())
}

/// Whether an accepted entry covers `id`'s `metric` at `new_value`. `None`
/// means "not accepted" (no entry, no matching `base_fingerprint`, or this
/// metric wasn't part of the accept) — the caller falls through to a normal
/// FAIL against the original reference value. `Some` carries the verdict
/// against the accepted value instead: a one-sided ceiling, so an
/// improvement past the accepted number always passes too.
pub fn allows(
    entries: &Entries,
    id: &str,
    metric: &str,
    base_fingerprint: &str,
    new_value: f64,
    limit_pct: f64,
) -> Option<bool> {
    let entry = entries.get(id)?;
    if entry.base_fingerprint != base_fingerprint {
        return None;
    }
    let accepted = *entry.metrics.get(metric)?;
    Some(new_value <= accepted + accepted * limit_pct / 100.0)
}

/// Whether `id` has an accepted entry whose `base_fingerprint` no longer
/// matches — the reference side moved past the reviewed commit, so `gate
/// accept` needs to run again.
pub fn is_stale(entries: &Entries, id: &str, base_fingerprint: &str) -> bool {
    entries
        .get(id)
        .is_some_and(|e| e.base_fingerprint != base_fingerprint)
}

/// The most recently accepted justification for `id`, if any.
pub fn latest_justification<'a>(entries: &'a Entries, id: &str) -> Option<&'a str> {
    entries
        .get(id)?
        .justifications
        .last()
        .map(|j| j.text.as_str())
}

/// Drop entries whose `head_fingerprint` matches the id's fingerprint in a
/// freshly-saved baseline `run` — the bench's workload hasn't changed since
/// the accept, and the baseline now carries the accepted cost as the norm,
/// so the override is no longer needed. Returns the pruned map and the
/// dropped ids, for the caller to log.
pub fn prune_absorbed(entries: &Entries, run: &Run) -> (Entries, Vec<String>) {
    let mut pruned = entries.clone();
    let mut removed = Vec::new();
    for (id, entry) in entries {
        if run
            .items
            .get(id)
            .is_some_and(|m| m.fingerprint == entry.head_fingerprint)
        {
            pruned.remove(id);
            removed.push(id.clone());
        }
    }
    (pruned, removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invoke::ItemMetrics;

    fn entry(base_fp: &str, head_fp: &str, metrics: &[(&str, f64)]) -> Entry {
        Entry {
            base_fingerprint: base_fp.into(),
            head_fingerprint: head_fp.into(),
            metrics: metrics.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            justifications: vec![Justification {
                text: "test".into(),
                accepted_unix: 1,
            }],
        }
    }

    #[test]
    fn read_write_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "soothfast-gate-lock-test-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut entries = Entries::new();
        entries.insert(
            "pkg::bench".into(),
            entry("base-fp", "head-fp", &[("instructions", 100.0)]),
        );
        write(&dir, &entries).unwrap();
        let read_back = read(&dir).unwrap();
        assert_eq!(read_back, entries);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_lockfile_is_empty() {
        let dir = std::env::temp_dir().join(format!(
            "soothfast-gate-lock-missing-{}-{}",
            std::process::id(),
            line!()
        ));
        assert_eq!(read(&dir).unwrap(), Entries::new());
    }

    #[test]
    fn allows_a_value_at_the_accepted_ceiling() {
        let mut entries = Entries::new();
        entries.insert(
            "pkg::bench".into(),
            entry("base-fp", "head-fp", &[("instructions", 100.0)]),
        );
        assert_eq!(
            allows(
                &entries,
                "pkg::bench",
                "instructions",
                "base-fp",
                105.0,
                5.0
            ),
            Some(true)
        );
    }

    #[test]
    fn allows_an_improvement_past_the_accepted_value() {
        // One-sided: a follow-up fix reclaiming most of the accepted delta
        // must not fail just because it's far from the accepted number.
        let mut entries = Entries::new();
        entries.insert(
            "pkg::bench".into(),
            entry("base-fp", "head-fp", &[("instructions", 100.0)]),
        );
        assert_eq!(
            allows(&entries, "pkg::bench", "instructions", "base-fp", 10.0, 5.0),
            Some(true)
        );
    }

    #[test]
    fn fails_beyond_the_accepted_ceiling() {
        let mut entries = Entries::new();
        entries.insert(
            "pkg::bench".into(),
            entry("base-fp", "head-fp", &[("instructions", 100.0)]),
        );
        assert_eq!(
            allows(
                &entries,
                "pkg::bench",
                "instructions",
                "base-fp",
                200.0,
                5.0
            ),
            Some(false)
        );
    }

    #[test]
    fn a_stale_base_fingerprint_is_not_accepted() {
        let mut entries = Entries::new();
        entries.insert(
            "pkg::bench".into(),
            entry("old-base-fp", "head-fp", &[("instructions", 100.0)]),
        );
        assert_eq!(
            allows(
                &entries,
                "pkg::bench",
                "instructions",
                "new-base-fp",
                105.0,
                5.0
            ),
            None
        );
    }

    #[test]
    fn an_unaccepted_metric_is_not_covered() {
        let mut entries = Entries::new();
        entries.insert(
            "pkg::bench".into(),
            entry("base-fp", "head-fp", &[("instructions", 100.0)]),
        );
        assert_eq!(
            allows(&entries, "pkg::bench", "allocs", "base-fp", 12.0, 5.0),
            None
        );
    }

    #[test]
    fn prune_drops_entries_absorbed_by_a_new_baseline() {
        let mut entries = Entries::new();
        entries.insert(
            "pkg::absorbed".into(),
            entry("base-fp", "new-head-fp", &[("instructions", 100.0)]),
        );
        entries.insert(
            "pkg::still-open".into(),
            entry("base-fp", "other-head-fp", &[("instructions", 50.0)]),
        );
        let mut run = Run::default();
        run.items.insert(
            "pkg::absorbed".into(),
            ItemMetrics {
                fingerprint: "new-head-fp".into(),
                ..Default::default()
            },
        );
        let (pruned, removed) = prune_absorbed(&entries, &run);
        assert_eq!(removed, vec!["pkg::absorbed".to_string()]);
        assert!(!pruned.contains_key("pkg::absorbed"));
        assert!(pruned.contains_key("pkg::still-open"));
    }
}
