//! The passes the default branch recorded, one small file each.

use std::path::PathBuf;
use std::time::SystemTime;

use serde_json::{Value, json};

use crate::runcache::{self, hashed};

/// Passes kept before the oldest are dropped.
const KEEP: usize = 2048;

/// Bytes the passes may take together. A pass holds its lane's baseline, and
/// the plan job restores the whole store on every run.
const KEEP_BYTES: u64 = 32 << 20;

/// The key a lane's pass is stored under. `base_delta` names how the base
/// the pass was judged against differs from the current one; it is empty
/// for a pass judged against the default branch itself.
pub fn key(lane: &str, fingerprint: &str, base_delta: &str) -> String {
    hashed(&[
        "soothfast-lane-v1".into(),
        lane.into(),
        fingerprint.into(),
        base_delta.into(),
    ])
}

/// Whether `key` has the shape [`key`] gives, which is safe to use as a file
/// name.
pub fn is_key(key: &str) -> bool {
    key.len() == 16 && key.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Where a lane passed, and the measurements the passing run left for the
/// lane's items. A pass stored without them cannot stand in for a measuring
/// job.
#[derive(Debug, Clone, PartialEq)]
pub struct Pass {
    pub commit: String,
    pub run: String,
    pub baseline: Option<Value>,
}

/// Passes of lanes, keyed by [`key`].
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// `.soothfast/lanes`, created on demand.
    pub fn open() -> Option<Store> {
        Some(Store {
            dir: runcache::state_dir("lanes")?,
        })
    }

    #[cfg(test)]
    pub(crate) fn at(dir: PathBuf) -> Store {
        Store { dir }
    }

    /// The pass stored for `lane` under `key`. A hit is rewritten so pruning
    /// drops passes nobody asks for first.
    pub fn lookup(&self, lane: &str, key: &str) -> Option<Pass> {
        let path = self.dir.join(key);
        let text = std::fs::read_to_string(&path).ok()?;
        let doc: Value = serde_json::from_str(&text).ok()?;
        if doc["lane"] != lane {
            return None;
        }
        let _ = std::fs::write(&path, text);
        Some(Pass {
            commit: doc["commit"].as_str()?.to_string(),
            run: doc["run"].as_str()?.to_string(),
            baseline: doc.get("baseline").cloned(),
        })
    }

    /// Store a pass of `lane` under `key`.
    ///
    /// Best effort: a pass that cannot be written costs a run, nothing else.
    pub fn record(&self, lane: &str, key: &str, pass: &Pass) {
        let mut doc = json!({"lane": lane, "commit": pass.commit, "run": pass.run});
        if let Some(baseline) = &pass.baseline {
            doc["baseline"] = baseline.clone();
        }
        if std::fs::write(self.dir.join(key), format!("{doc}\n")).is_ok() {
            self.prune();
        }
    }

    /// Drops the oldest passes past [`KEEP`] files or [`KEEP_BYTES`].
    fn prune(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut files: Vec<(SystemTime, PathBuf, u64)> = entries
            .flatten()
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                Some((meta.modified().ok()?, e.path(), meta.len()))
            })
            .collect();
        files.sort_by_key(|(time, ..)| std::cmp::Reverse(*time));
        let mut bytes = 0;
        for (i, (_, path, len)) in files.into_iter().enumerate() {
            bytes += len;
            if i >= KEEP || bytes > KEEP_BYTES {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::fixture::TempDir;
    use std::time::{Duration, SystemTime};

    fn pass(commit: &str) -> Pass {
        Pass {
            commit: commit.into(),
            run: "7".into(),
            baseline: None,
        }
    }

    fn store(dir: &TempDir) -> Store {
        std::fs::create_dir_all(dir).unwrap();
        Store::at(dir.to_path_buf())
    }

    fn set_modified(path: &std::path::Path, at: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }

    #[test]
    fn a_recorded_pass_reads_back() {
        let dir = TempDir::new("lane-store-round");
        let s = store(&dir);
        let k = key("rust:core", "f1", "");
        assert_eq!(s.lookup("rust:core", &k), None);
        s.record("rust:core", &k, &pass("abc"));
        assert_eq!(s.lookup("rust:core", &k), Some(pass("abc")));
    }

    #[test]
    fn a_baseline_reads_back_and_an_old_pass_has_none() {
        let dir = TempDir::new("lane-store-baseline");
        let s = store(&dir);
        let k = key("rust:core", "f1", "");
        let with = Pass {
            baseline: Some(json!({"items": {"a": {}}})),
            ..pass("abc")
        };
        s.record("rust:core", &k, &with);
        assert_eq!(s.lookup("rust:core", &k), Some(with));
        let old = key("rust:core", "f2", "");
        std::fs::write(
            dir.join(&old),
            r#"{"lane":"rust:core","commit":"abc","run":"7"}"#,
        )
        .unwrap();
        assert_eq!(s.lookup("rust:core", &old), Some(pass("abc")));
    }

    #[test]
    fn recording_prunes_the_oldest_past_the_byte_limit() {
        let dir = TempDir::new("lane-store-bytes");
        let s = store(&dir);
        let base = SystemTime::now() - Duration::from_secs(10_000);
        let big = "x".repeat(KEEP_BYTES as usize / 4);
        for i in 0..4 {
            let path = dir.join(format!("old{i}"));
            std::fs::write(&path, &big).unwrap();
            set_modified(&path, base + Duration::from_secs(i));
        }
        s.record("docs", "fresh", &pass("abc"));
        assert!(dir.join("fresh").exists());
        assert!(dir.join("old3").exists());
        assert!(!dir.join("old0").exists());
    }

    #[test]
    fn the_lane_and_the_fingerprint_and_the_base_delta_move_the_key() {
        let k = key("rust:core", "f1", "");
        assert_eq!(k, key("rust:core", "f1", ""));
        assert_ne!(k, key("rust:other", "f1", ""));
        assert_ne!(k, key("rust:core", "f2", ""));
        assert_ne!(k, key("rust:core", "f1", "delta"));
    }

    #[test]
    fn only_a_digest_is_a_key() {
        assert!(is_key(&key("rust:core", "f1", "")));
        for bad in ["", "../x", "k-docs", "ABCDEF0123456789", "0123456789abcde/"] {
            assert!(!is_key(bad), "{bad}");
        }
    }

    #[test]
    fn a_file_for_another_lane_is_not_a_hit() {
        let dir = TempDir::new("lane-store-guard");
        let s = store(&dir);
        let k = key("rust:core", "f1", "");
        s.record("rust:core", &k, &pass("abc"));
        assert_eq!(s.lookup("docs", &k), None);
    }

    #[test]
    fn a_hit_renews_the_file_so_pruning_keeps_it() {
        let dir = TempDir::new("lane-store-renew");
        let s = store(&dir);
        let k = key("docs", "f1", "");
        s.record("docs", &k, &pass("abc"));
        let path = dir.join(&k);
        let old = SystemTime::now() - Duration::from_secs(3600);
        set_modified(&path, old);
        assert!(s.lookup("docs", &k).is_some());
        let renewed = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(renewed > old);
    }

    #[test]
    fn recording_prunes_the_oldest_past_the_limit() {
        let dir = TempDir::new("lane-store-prune");
        let s = store(&dir);
        let base = SystemTime::now() - Duration::from_secs(10_000);
        for i in 0..KEEP {
            let path = dir.join(format!("old{i}"));
            std::fs::write(&path, "x").unwrap();
            set_modified(&path, base + Duration::from_secs(i as u64));
        }
        s.record("docs", "fresh", &pass("abc"));
        assert_eq!(std::fs::read_dir(&*dir).unwrap().count(), KEEP);
        assert!(dir.join("fresh").exists());
        assert!(!dir.join("old0").exists());
    }
}
