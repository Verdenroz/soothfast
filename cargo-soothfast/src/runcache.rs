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

/// The conditions every run in one gate invocation is keyed under. Without a
/// resolved backend nothing is loaded or stored.
pub struct Runs<'a> {
    stamp: &'a BuildStamp,
    common: &'a CommonArgs,
    backend: Option<String>,
}

impl<'a> Runs<'a> {
    /// `backend` is the gating backend the bench binary resolves on this host,
    /// not the `--backend` argument, which may be `auto`.
    pub fn new(stamp: &'a BuildStamp, common: &'a CommonArgs, backend: Option<String>) -> Self {
        Runs {
            stamp,
            common,
            backend,
        }
    }

    /// A stored run measured from `id` (a commit or a binary digest), tagged
    /// with `measured_from`.
    pub fn load(&self, id: &str, measured_from: &str) -> Option<Value> {
        let backend = self.backend.as_deref()?;
        load(&key(id, self.stamp, self.common, backend), measured_from)
    }

    /// Keep `doc` as the measurement of `id`.
    pub fn store(&self, id: &str, doc: &Value) {
        if let Some(backend) = self.backend.as_deref() {
            store(&key(id, self.stamp, self.common, backend), doc);
        }
    }
}

/// Identity of a measurement: the commit, plus everything outside the commit
/// that changes the numbers.
///
/// The reference tree's own `[profile.*]` follows from the commit, so it is
/// not hashed here. It still travels in the stored run's build stamp, where
/// the gate's normal comparison sees it. `backend` is what measured, so a
/// perfcnt run is never served to a gate that resolved callgrind, or the
/// reverse; the `--backend` argument stays too, since it decides which
/// non-gating backends ran alongside.
fn key(id: &str, stamp: &BuildStamp, common: &CommonArgs, backend: &str) -> String {
    let scope = [
        id,
        &stamp.rustc,
        &stamp.codegen_units,
        &stamp.rustflags,
        &cpu_model(),
        &invoke::harness_versions(),
        common.pkg.as_deref().unwrap_or(""),
        common.features.as_deref().unwrap_or(""),
        common.target.as_deref().unwrap_or(""),
        common.backend.as_deref().unwrap_or(""),
        backend,
        common.samples.as_deref().unwrap_or(""),
        common.filter.as_deref().unwrap_or(""),
    ]
    .join("\u{1}");
    format!("{:016x}", soothfast_registry::fnv1a(scope.as_bytes()))
}

fn load(key: &str, measured_from: &str) -> Option<Value> {
    let text = std::fs::read_to_string(dir()?.join(format!("{key}.json"))).ok()?;
    let mut doc: Value = serde_json::from_str(&text).ok()?;
    doc["reused_from"] = serde_json::json!(measured_from);
    Some(doc)
}

/// Best effort: a cache that cannot be written costs a re-measurement,
/// nothing else.
fn store(key: &str, doc: &Value) {
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
        assert_ne!(
            key("aaa", &stamp(), &args(), "callgrind"),
            key("bbb", &stamp(), &args(), "callgrind")
        );
    }

    #[test]
    fn build_conditions_are_part_of_the_identity() {
        let mut other = stamp();
        other.codegen_units = "16".into();
        assert_ne!(
            key("aaa", &stamp(), &args(), "callgrind"),
            key("aaa", &other, &args(), "callgrind")
        );
    }

    #[test]
    fn measurement_scope_is_part_of_the_identity() {
        let mut other = args();
        other.features = Some("full".into());
        assert_ne!(
            key("aaa", &stamp(), &args(), "callgrind"),
            key("aaa", &stamp(), &other, "callgrind")
        );
    }

    #[test]
    fn the_resolved_backend_is_part_of_the_identity() {
        assert_ne!(
            key("aaa", &stamp(), &args(), "perfcnt"),
            key("aaa", &stamp(), &args(), "callgrind")
        );
    }

    #[test]
    fn an_unresolved_backend_loads_and_stores_nothing() {
        let (stamp, args) = (stamp(), args());
        let doc = serde_json::json!({ "version": 1, "items": {} });
        Runs::new(&stamp, &args, None).store("test-runcache-unresolved", &doc);
        assert!(
            Runs::new(&stamp, &args, None)
                .load("test-runcache-unresolved", "x")
                .is_none()
        );
        let stored = key("test-runcache-unresolved", &stamp, &args, "");
        assert!(load(&stored, "x").is_none());
    }

    #[test]
    fn the_reference_trees_profiles_are_not_hashed() {
        let mut other = stamp();
        other.profiles = "zzzz".into();
        assert_eq!(
            key("aaa", &stamp(), &args(), "callgrind"),
            key("aaa", &other, &args(), "callgrind")
        );
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
