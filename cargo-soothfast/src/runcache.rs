//! Measured runs kept on disk, keyed by the commit and the conditions they
//! were measured under.
//!
//! Measuring the reference side is the most expensive thing the gate does,
//! and it repeats: the next push to a branch has the same merge-base, and on
//! master the commit gated as HEAD becomes the next commit's reference.
//! Reuse is safe because the key pins every condition that moves the
//! numbers, so a run measured any other way cannot be served from here.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::buildstamp::BuildStamp;
use crate::invoke::{self, CommonArgs, HostEnv, Shard};

/// Runs kept before the oldest are dropped. A gate leaves two live entries
/// per branch, so this holds many branches at a bounded size.
pub(crate) const KEEP: usize = 32;

/// `.soothfast/<name>`, created on demand.
pub fn state_dir(name: &str) -> Option<PathBuf> {
    let d = invoke::project_root().join(".soothfast").join(name);
    std::fs::create_dir_all(&d).ok()?;
    Some(d)
}

fn dir() -> Option<PathBuf> {
    state_dir("runs")
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

    /// What the bench binary resolved on this host, when it answered.
    pub fn env(&self) -> Option<&HostEnv> {
        self.env.as_ref()
    }

    /// The gating backend the bench binary resolved, when it answered.
    pub fn gating_backend(&self) -> Option<&str> {
        self.env.as_ref().map(|e| e.gating_backend.as_str())
    }

    /// A stored run measured from `id` (a commit or a binary digest), tagged
    /// with `measured_from`.
    pub fn load(&self, id: &str, measured_from: &str) -> Option<Value> {
        let env = self.env.as_ref()?;
        load(&key(id, self.stamp, self.common, env), measured_from)
    }

    /// [`Runs::load`] for the reference side of a gate. A shard also takes the
    /// run of every item there, since compare reads only the items it owns.
    pub fn load_reference(&self, id: &str, measured_from: &str) -> Option<Value> {
        self.reference_source(id, measured_from).map(|(_, doc)| doc)
    }

    /// A stored run of `id` that may hold every item this invocation
    /// measures: its own key first, then the run taken without the shard,
    /// without the filter, and without either. The caller checks it holds
    /// the items it needs.
    pub fn load_covering(&self, id: &str, measured_from: &str) -> Option<Value> {
        let env = self.env.as_ref()?;
        let (shard, filter) = (self.common.shard, self.common.filter.as_deref());
        let mut keys: Vec<String> = Vec::new();
        for (shard, filter) in [(shard, filter), (None, filter), (shard, None), (None, None)] {
            let key = key_with(id, self.stamp, self.common, env, shard, filter);
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        keys.iter().find_map(|key| load(key, measured_from))
    }

    /// The run and its file that a gate with `reuse` takes in place of
    /// measuring the merge-base at `base_sha`. The gate and `prebuild` both
    /// ask here, so they agree on whether the merge-base needs building.
    pub fn reusable_reference(&self, reuse: bool, base_sha: &str) -> Option<(PathBuf, Value)> {
        if !reuse || base_sha.is_empty() {
            return None;
        }
        self.reference_source(base_sha, base_sha)
    }

    fn reference_source(&self, id: &str, measured_from: &str) -> Option<(PathBuf, Value)> {
        let env = self.env.as_ref().filter(|_| !id.is_empty())?;
        let whole = self.common.shard.map(|_| {
            key_with(
                id,
                self.stamp,
                self.common,
                env,
                None,
                self.common.filter.as_deref(),
            )
        });
        [Some(key(id, self.stamp, self.common, env)), whole]
            .into_iter()
            .flatten()
            .find_map(|key| Some((run_path(&key)?, load(&key, measured_from)?)))
    }

    /// The key every per-item entry of this invocation lives under: the
    /// conditions of a run, without the commit or the filter.
    pub fn item_scope(&self) -> Option<String> {
        let env = self.env.as_ref()?;
        Some(hashed(&conditions(self.stamp, self.common, env)))
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
/// Callgrind counts also depend on how the harness takes them, so the method
/// is part of the key and a count is never reused across methods.
pub fn host(env: &HostEnv) -> String {
    match (env.gating_backend.as_str(), &env.guest) {
        ("callgrind", Some(g)) => format!(
            "guest {} glibc {} {} {} method {}",
            g.cpu,
            g.glibc,
            g.libc,
            g.valgrind,
            env.method()
        ),
        ("callgrind", None) => format!("{} method {}", cpu_model(), env.method()),
        _ => cpu_model(),
    }
}

pub fn measured_on(env: &HostEnv) -> Value {
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
    if env.gating_backend == "callgrind" {
        on["callgrind_method"] = serde_json::json!(env.method());
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
    key_with(
        id,
        stamp,
        common,
        env,
        common.shard,
        common.filter.as_deref(),
    )
}

fn key_with(
    id: &str,
    stamp: &BuildStamp,
    common: &CommonArgs,
    env: &HostEnv,
    shard: Option<Shard>,
    filter: Option<&str>,
) -> String {
    let mut parts = vec![id.to_string()];
    parts.extend(conditions(stamp, common, env));
    parts.push(filter.unwrap_or_default().to_string());
    if let Some(shard) = shard {
        parts.push(format!("shard {shard}"));
    }
    hashed(&parts)
}

/// What decides a measurement besides the code and the item filter.
fn conditions(stamp: &BuildStamp, common: &CommonArgs, env: &HostEnv) -> Vec<String> {
    [
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
    ]
    .map(String::from)
    .to_vec()
}

/// `parts` joined and hashed, as sixteen hex digits.
pub(crate) fn hashed(parts: &[String]) -> String {
    hex_fnv(parts.join("\u{1}").as_bytes())
}

/// `bytes` hashed, as sixteen hex digits.
pub(crate) fn hex_fnv(bytes: &[u8]) -> String {
    format!("{:016x}", soothfast_registry::fnv1a(bytes))
}

fn run_path(key: &str) -> Option<PathBuf> {
    Some(dir()?.join(format!("{key}.json")))
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
        prune_oldest(&d, KEEP);
    }
}

/// Drop the oldest files of `d` past `keep`.
pub fn prune_oldest(d: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(d) else {
        return;
    };
    drop_oldest(entries.flatten().filter_map(modified).collect(), keep);
}

/// A directory entry's modification time and path.
pub fn modified(e: std::fs::DirEntry) -> Option<(SystemTime, PathBuf)> {
    let t = e.metadata().ok()?.modified().ok()?;
    Some((t, e.path()))
}

/// Remove all but the `keep` newest of `files`.
pub fn drop_oldest(mut files: Vec<(SystemTime, PathBuf)>, keep: usize) {
    files.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    for (_, path) in files.into_iter().skip(keep) {
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
            callgrind_method: None,
            item_reuse: true,
        }
    }

    fn callgrind_guest() -> HostEnv {
        HostEnv {
            gating_backend: "callgrind".into(),
            item_reuse: true,
            callgrind_method: Some("one-process".into()),
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
    fn a_run_key_hashes_the_joined_conditions_in_order() {
        let (stamp, args, env) = (stamp(), args(), env("callgrind"));
        let joined = [
            "aaa",
            &stamp.rustc,
            &stamp.codegen_units,
            &stamp.rustflags,
            &host(&env),
            &invoke::harness_versions(),
            "demo",
            "",
            "",
            "",
            "callgrind",
            "",
            "",
        ]
        .join("\u{1}");
        assert_eq!(
            key("aaa", &stamp, &args, &env),
            format!("{:016x}", soothfast_registry::fnv1a(joined.as_bytes()))
        );
    }

    #[test]
    fn an_item_scope_ignores_the_filter_but_not_the_package() {
        let (stamp, mut other) = (stamp(), args());
        let scope = |a: &CommonArgs| {
            Runs::new(&stamp, a, Some(env("callgrind")))
                .item_scope()
                .expect("scope")
        };
        let base = scope(&args());
        other.filter = Some("stats".into());
        assert_eq!(base, scope(&other));
        other.pkg = Some("other".into());
        assert_ne!(base, scope(&other));
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
            "guest 0123456789abcdef glibc 2.39 fedcba9876543210 valgrind-3.22.0 method one-process"
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
        assert_eq!(
            host(&env("callgrind")),
            format!("{} method two-process", cpu_model())
        );
    }

    #[test]
    fn a_count_is_never_keyed_the_same_across_callgrind_methods() {
        let one = callgrind_guest();
        let mut two = callgrind_guest();
        two.callgrind_method = Some("two-process".into());
        let mut unnamed = callgrind_guest();
        unnamed.callgrind_method = None;
        let key = |env: &HostEnv| key("aaa", &stamp(), &args(), env);
        assert_ne!(key(&one), key(&two));
        assert_eq!(key(&two), key(&unnamed));
        assert_ne!(
            Runs::new(&stamp(), &args(), Some(one)).item_scope(),
            Runs::new(&stamp(), &args(), Some(two)).item_scope()
        );
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
        assert_eq!(got["measured_on"]["callgrind_method"], "one-process");
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
