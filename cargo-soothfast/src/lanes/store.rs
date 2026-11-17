//! The passes the default branch recorded, one small file each.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::runcache::{self, hashed};

/// Passes kept before the oldest are dropped.
const KEEP: usize = 2048;

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

/// Where a lane passed.
#[derive(Debug, PartialEq)]
pub struct Pass {
    pub commit: String,
    pub run: String,
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
        })
    }

    /// Store a pass of `lane` under `key`.
    ///
    /// Best effort: a pass that cannot be written costs a run, nothing else.
    pub fn record(&self, lane: &str, key: &str, pass: &Pass) {
        let doc = json!({"lane": lane, "commit": pass.commit, "run": pass.run});
        if std::fs::write(self.dir.join(key), format!("{doc}\n")).is_ok() {
            runcache::prune_oldest(&self.dir, KEEP);
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
    fn the_lane_and_the_fingerprint_and_the_base_delta_move_the_key() {
        let k = key("rust:core", "f1", "");
        assert_eq!(k, key("rust:core", "f1", ""));
        assert_ne!(k, key("rust:other", "f1", ""));
        assert_ne!(k, key("rust:core", "f2", ""));
        assert_ne!(k, key("rust:core", "f1", "delta"));
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
