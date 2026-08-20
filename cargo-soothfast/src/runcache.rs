//! Measured runs kept on disk, keyed by the commit and the conditions they
//! were measured under.
//!
//! Measuring the reference side is the most expensive thing the gate does,
//! and it repeats: the next push to a branch has the same merge-base, and on
//! master the commit gated as HEAD becomes the next commit's reference.
//! Reuse is safe because the key pins every condition that moves the
//! numbers, so a run measured any other way cannot be served from here.

use std::path::PathBuf;

use serde_json::Value;

use crate::buildstamp::BuildStamp;
use crate::invoke::{self, CommonArgs};

/// Runs kept before the oldest are dropped. A gate leaves two live entries
/// per branch, so this holds many branches at a bounded size.
const KEEP: usize = 32;

fn dir() -> Option<PathBuf> {
    let d = invoke::workspace_root()
        .ok()?
        .join(".soothfast")
        .join("runs");
    std::fs::create_dir_all(&d).ok()?;
    Some(d)
}

/// Identity of a measurement: the commit, plus everything outside the commit
/// that changes the numbers.
///
/// The reference tree's own `[profile.*]` follows from the commit, so it is
/// not hashed here. It still travels in the stored run's build stamp, where
/// the gate's normal comparison sees it.
pub fn key(sha: &str, stamp: &BuildStamp, common: &CommonArgs) -> String {
    let scope = [
        sha,
        &stamp.rustc,
        &stamp.codegen_units,
        &stamp.rustflags,
        &cpu_model(),
        &invoke::harness_versions(),
        common.pkg.as_deref().unwrap_or(""),
        common.features.as_deref().unwrap_or(""),
        common.target.as_deref().unwrap_or(""),
        common.backend.as_deref().unwrap_or(""),
        common.samples.as_deref().unwrap_or(""),
        common.filter.as_deref().unwrap_or(""),
    ]
    .join("\u{1}");
    format!("{:016x}", soothfast_registry::fnv1a(scope.as_bytes()))
}

/// A stored run for `key`, tagged with the commit it measured.
pub fn load(key: &str, sha: &str) -> Option<Value> {
    let text = std::fs::read_to_string(dir()?.join(format!("{key}.json"))).ok()?;
    let mut doc: Value = serde_json::from_str(&text).ok()?;
    doc["reused_from"] = serde_json::json!(sha);
    Some(doc)
}

/// Keep `doc` as the measurement of `sha`. Best effort: a cache that cannot
/// be written costs a re-measurement, nothing else.
pub fn store(key: &str, doc: &Value) {
    let Some(d) = dir() else {
        return;
    };
    if std::fs::write(d.join(format!("{key}.json")), doc.to_string()).is_ok() {
        prune(&d);
    }
}

fn prune(d: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(d) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            let t = e.metadata().ok()?.modified().ok()?;
            Some((t, e.path()))
        })
        .collect();
    if files.len() <= KEEP {
        return;
    }
    files.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    for (_, path) in files.drain(KEEP..) {
        let _ = std::fs::remove_file(path);
    }
}

/// Retired instruction counts differ across microarchitectures, so a run
/// measured on another machine is not interchangeable with this one's.
fn cpu_model() -> String {
    let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") else {
        return String::new();
    };
    text.lines()
        .find_map(|l| l.strip_prefix("model name"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp() -> BuildStamp {
        BuildStamp {
            rustc: "1.88.0 x86_64-unknown-linux-gnu".into(),
            codegen_units: "1".into(),
            profiles: "aaaa".into(),
            rustflags: "bbbb".into(),
        }
    }

    fn args() -> CommonArgs {
        CommonArgs {
            pkg: Some("demo".into()),
            ..Default::default()
        }
    }

    #[test]
    fn the_commit_is_part_of_the_identity() {
        assert_ne!(key("aaa", &stamp(), &args()), key("bbb", &stamp(), &args()));
    }

    #[test]
    fn build_conditions_are_part_of_the_identity() {
        let mut other = stamp();
        other.codegen_units = "16".into();
        assert_ne!(key("aaa", &stamp(), &args()), key("aaa", &other, &args()));
    }

    #[test]
    fn measurement_scope_is_part_of_the_identity() {
        let mut other = args();
        other.features = Some("full".into());
        assert_ne!(key("aaa", &stamp(), &args()), key("aaa", &stamp(), &other));
    }

    #[test]
    fn the_reference_trees_profiles_are_not_hashed() {
        let mut other = stamp();
        other.profiles = "zzzz".into();
        assert_eq!(key("aaa", &stamp(), &args()), key("aaa", &other, &args()));
    }

    #[test]
    fn a_loaded_run_names_the_commit_it_measured() {
        let doc = serde_json::json!({ "version": 1, "items": {} });
        store("test-runcache-load", &doc);
        let got = load("test-runcache-load", "deadbeef").expect("stored run");
        assert_eq!(got["reused_from"], "deadbeef");
        if let Some(d) = dir() {
            let _ = std::fs::remove_file(d.join("test-runcache-load.json"));
        }
    }
}
