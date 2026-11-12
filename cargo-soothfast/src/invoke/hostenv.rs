//! `HostEnv`: what a bench binary resolves on this host, and its item listing.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// What a bench binary resolves on this host, from the env line a run starts
/// with.
#[derive(Debug, Clone, PartialEq)]
pub struct HostEnv {
    pub gating_backend: String,
    /// Set only when callgrind gates and the harness can see its guest.
    pub guest: Option<Guest>,
    /// How callgrind's per-iteration Ir is taken, when callgrind gates. A
    /// harness that does not name it only has the two-process method.
    pub callgrind_method: Option<String>,
    /// Whether the harness takes `--id` and reports `executed` records.
    pub item_reuse: bool,
}

/// What callgrind's counts depend on besides the binary.
#[derive(Debug, Clone, PartialEq)]
pub struct Guest {
    pub cpu: String,
    pub glibc: String,
    /// Digest of the loaded libc file; distro rebuilds keep `glibc`.
    pub libc: String,
    pub valgrind: String,
}

impl HostEnv {
    /// The method callgrind's counts come from.
    pub fn method(&self) -> &str {
        self.callgrind_method.as_deref().unwrap_or("two-process")
    }

    pub(super) fn from_line(v: &Value) -> Option<Self> {
        let field = |k: &str| v[k].as_str().map(String::from);
        let guest = match (
            field("guest_cpu"),
            field("guest_glibc"),
            field("guest_libc"),
            field("valgrind"),
        ) {
            (Some(cpu), Some(glibc), Some(libc), Some(valgrind)) => Some(Guest {
                cpu,
                glibc,
                libc,
                valgrind,
            }),
            _ => None,
        };
        Some(HostEnv {
            gating_backend: field("gating_backend")?,
            guest,
            callgrind_method: field("callgrind_method"),
            item_reuse: v["item_reuse"].as_bool().unwrap_or(false),
        })
    }
}

/// What `exe` resolves on this host for `backend`, as the env line of a real
/// run would name it. A harness older than `--env`, or a named backend this
/// host cannot run, exits non-zero and reads as unresolved.
pub fn host_env(exe: &Path, backend: Option<&str>) -> Result<HostEnv, String> {
    let mut cmd = Command::new(exe);
    cmd.arg("--env");
    if let Some(b) = backend {
        cmd.args(["--backend", b]);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("could not run the bench harness: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = stderr.lines().last().unwrap_or("").trim();
        return Err(format!("bench harness does not answer --env ({why})"));
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["type"] == "env")
        .and_then(|v| HostEnv::from_line(&v))
        .ok_or_else(|| "bench harness printed no env line for --env".into())
}

/// The items `exe` registers with their fingerprints, narrowed by `filter`
/// the way a run is.
pub fn list_items(exe: &Path, filter: Option<&str>) -> Option<BTreeMap<String, String>> {
    let mut cmd = Command::new(exe);
    cmd.args(["--list", "--json"]);
    if let Some(f) = filter {
        cmd.args(["--filter", f]);
    }
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["type"] == "item")
            .filter_map(|v| {
                Some((
                    v["id"].as_str()?.to_string(),
                    v["fingerprint"].as_str()?.to_string(),
                ))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::{Guest, HostEnv};

    #[test]
    fn a_harness_advertises_item_reuse_in_its_env_line() {
        let line = |v: serde_json::Value| HostEnv::from_line(&v).map(|e| e.item_reuse);
        assert_eq!(
            line(serde_json::json!({ "gating_backend": "callgrind", "item_reuse": true })),
            Some(true)
        );
        assert_eq!(
            line(serde_json::json!({ "gating_backend": "callgrind" })),
            Some(false)
        );
    }

    #[test]
    fn a_harness_without_a_named_method_measures_two_process() {
        let line = serde_json::json!({ "type": "env", "gating_backend": "callgrind" });
        assert_eq!(
            HostEnv::from_line(&line)
                .map(|e| e.method().to_string())
                .as_deref(),
            Some("two-process")
        );
    }

    #[test]
    fn an_env_line_reads_its_guest_only_when_complete() {
        let full = serde_json::json!({
            "type": "env", "gating_backend": "callgrind",
            "guest_cpu": "0123456789abcdef", "guest_glibc": "2.39",
            "guest_libc": "fedcba9876543210", "valgrind": "valgrind-3.22.0",
            "callgrind_method": "one-process",
        });
        assert_eq!(
            HostEnv::from_line(&full),
            Some(HostEnv {
                gating_backend: "callgrind".into(),
                guest: Some(Guest {
                    cpu: "0123456789abcdef".into(),
                    glibc: "2.39".into(),
                    libc: "fedcba9876543210".into(),
                    valgrind: "valgrind-3.22.0".into(),
                }),
                callgrind_method: Some("one-process".into()),
                item_reuse: false,
            })
        );
        let partial = serde_json::json!({
            "type": "env", "gating_backend": "callgrind", "guest_cpu": "0123456789abcdef",
        });
        assert_eq!(HostEnv::from_line(&partial).and_then(|e| e.guest), None);
        assert_eq!(
            HostEnv::from_line(&serde_json::json!({ "type": "env" })),
            None
        );
    }
}
