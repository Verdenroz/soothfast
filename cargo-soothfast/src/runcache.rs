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
use crate::invoke::{self, CommonArgs, HostEnv};

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
    env: Option<HostEnv>,
}

impl<'a> Runs<'a> {
    /// `env` is what the bench binary resolves on this host, not the
    /// `--backend` argument, which may be `auto`.
    pub fn new(stamp: &'a BuildStamp, common: &'a CommonArgs, env: Option<HostEnv>) -> Self {
        Runs { stamp, common, env }
    }

    /// A stored run measured from `id` (a commit or a binary digest), tagged
    /// with `measured_from`.
    pub fn load(&self, id: &str, measured_from: &str) -> Option<Value> {
        let env = self.env.as_ref()?;
        load(&key(id, self.stamp, self.common, env), measured_from)
    }

    /// Keep `doc` as the measurement of `id`, with the host conditions its
    /// key hashes, so a miss can be explained by diffing two stored runs.
    pub fn store(&self, id: &str, doc: &Value) {
        let Some(env) = self.env.as_ref() else {
            return;
        };
        let mut doc = doc.clone();
        doc["measured_on"] = measured_on(env);
        store(&key(id, self.stamp, self.common, env), &doc);
    }
}

/// The host half of the key. Callgrind counts the guest valgrind presents,
/// which it synthesizes the same on different host models, so it keys on that
/// guest and the glibc and valgrind that shape its instruction stream. Every
/// other backend reads the host itself, so it keys on the model name.
pub fn host(env: &HostEnv) -> String {
    match (env.gating_backend.as_str(), &env.guest) {
        ("callgrind", Some(g)) => format!(
            "guest {} glibc {} {} {}",
            g.cpu, g.glibc, g.libc, g.valgrind
        ),
        _ => cpu_model(),
    }
}

fn measured_on(env: &HostEnv) -> Value {
    let mut on = serde_json::json!({
        "gating_backend": env.gating_backend,
        "cpu_model": cpu_model(),
    });
    if let Some(g) = &env.guest {
        on["guest_cpu"] = serde_json::json!(g.cpu);
        on["guest_glibc"] = serde_json::json!(g.glibc);
        on["guest_libc"] = serde_json::json!(g.libc);
        on["valgrind"] = serde_json::json!(g.valgrind);
    }
    on
}

/// Identity of a measurement: the commit, plus everything outside the commit
/// that changes the numbers.
///
/// The reference tree's own `[profile.*]` follows from the commit, so it is
/// not hashed here. It still travels in the stored run's build stamp, where
/// the gate's normal comparison sees it. The resolved backend is what
/// measured, so a perfcnt run is never served to a gate that resolved
/// callgrind, or the reverse; the `--backend` argument stays too, since it
/// decides which non-gating backends ran alongside.
fn key(id: &str, stamp: &BuildStamp, common: &CommonArgs, env: &HostEnv) -> String {
    let scope = [
        id,
        &stamp.rustc,
        &stamp.codegen_units,
        &stamp.rustflags,
        &host(env),
        &invoke::harness_versions(),
        common.pkg.as_deref().unwrap_or(""),
        common.features.as_deref().unwrap_or(""),
        common.target.as_deref().unwrap_or(""),
        common.backend.as_deref().unwrap_or(""),
        &env.gating_backend,
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

    fn env(backend: &str) -> HostEnv {
        HostEnv {
            gating_backend: backend.into(),
            guest: None,
        }
    }

    fn callgrind_guest() -> HostEnv {
        HostEnv {
            gating_backend: "callgrind".into(),
            guest: Some(invoke::Guest {
                cpu: "0123456789abcdef".into(),
                glibc: "2.39".into(),
                libc: "fedcba9876543210".into(),
                valgrind: "valgrind-3.22.0".into(),
            }),
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
            key("aaa", &stamp(), &args(), &env("callgrind")),
            key("bbb", &stamp(), &args(), &env("callgrind"))
        );
    }

    #[test]
    fn build_conditions_are_part_of_the_identity() {
        let mut other = stamp();
        other.codegen_units = "16".into();
        assert_ne!(
            key("aaa", &stamp(), &args(), &env("callgrind")),
            key("aaa", &other, &args(), &env("callgrind"))
        );
    }

    #[test]
    fn measurement_scope_is_part_of_the_identity() {
        let mut other = args();
        other.features = Some("full".into());
        assert_ne!(
            key("aaa", &stamp(), &args(), &env("callgrind")),
            key("aaa", &stamp(), &other, &env("callgrind"))
        );
    }

    #[test]
    fn the_resolved_backend_is_part_of_the_identity() {
        assert_ne!(
            key("aaa", &stamp(), &args(), &env("perfcnt")),
            key("aaa", &stamp(), &args(), &env("callgrind"))
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
        let stored = key("test-runcache-unresolved", &stamp, &args, &env("callgrind"));
        assert!(load(&stored, "x").is_none());
    }

    #[test]
    fn callgrind_keys_on_its_guest_not_the_host_model() {
        assert_eq!(
            host(&callgrind_guest()),
            "guest 0123456789abcdef glibc 2.39 fedcba9876543210 valgrind-3.22.0"
        );
    }

    #[test]
    fn the_guest_cpu_libc_and_valgrind_are_each_part_of_the_identity() {
        let base = key("aaa", &stamp(), &args(), &callgrind_guest());
        let edits: [fn(&mut invoke::Guest); 4] = [
            |g| g.cpu = "fedcba9876543210".into(),
            |g| g.glibc = "2.41".into(),
            |g| g.libc = "0000000000000001".into(),
            |g| g.valgrind = "valgrind-3.24.0".into(),
        ];
        for edit in edits {
            let mut other = callgrind_guest();
            edit(other.guest.as_mut().expect("guest"));
            assert_ne!(base, key("aaa", &stamp(), &args(), &other));
        }
    }

    #[test]
    fn other_backends_and_a_silent_guest_key_on_the_host_model() {
        let mut perf = callgrind_guest();
        perf.gating_backend = "perfcnt".into();
        assert_eq!(host(&perf), cpu_model());
        assert_eq!(host(&env("callgrind")), cpu_model());
    }

    #[test]
    fn a_stored_run_records_the_conditions_it_was_keyed_on() {
        let (stamp, args, env) = (stamp(), args(), callgrind_guest());
        let doc = serde_json::json!({ "version": 1, "items": {} });
        let runs = Runs::new(&stamp, &args, Some(env));
        runs.store("test-runcache-measured-on", &doc);
        let got = runs
            .load("test-runcache-measured-on", "x")
            .expect("stored run");
        assert_eq!(got["measured_on"]["gating_backend"], "callgrind");
        assert_eq!(got["measured_on"]["guest_cpu"], "0123456789abcdef");
        assert_eq!(got["measured_on"]["guest_glibc"], "2.39");
        assert_eq!(got["measured_on"]["guest_libc"], "fedcba9876543210");
        assert_eq!(got["measured_on"]["valgrind"], "valgrind-3.22.0");
        if let Some(d) = dir() {
            let key = key(
                "test-runcache-measured-on",
                &stamp,
                &args,
                &callgrind_guest(),
            );
            let _ = std::fs::remove_file(d.join(format!("{key}.json")));
        }
    }

    #[test]
    fn the_reference_trees_profiles_are_not_hashed() {
        let mut other = stamp();
        other.profiles = "zzzz".into();
        assert_eq!(
            key("aaa", &stamp(), &args(), &env("callgrind")),
            key("aaa", &other, &args(), &env("callgrind"))
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
