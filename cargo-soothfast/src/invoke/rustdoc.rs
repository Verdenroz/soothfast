//! rustdoc JSON building, its toolchain trouble diagnosis, and the store keys
//! it is cached under.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};

use serde_json::Value;

use super::{target_dir, timed, workspace_root};
use crate::{rustdoc_inputs, rustdocstore, store};

/// Which items rustdoc should emit.
///
/// The docs engine wants the public surface and nothing else. Spec
/// generation needs the opposite: route handlers are commonly not `pub`, and
/// rustdoc omits private items entirely, so their signatures — and the
/// private fields of types they reference — would simply be missing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
    /// Public API only.
    Public,
    /// Include private items.
    Private,
}

/// rustdoc JSON schema this build was developed against.
///
/// The format is explicitly unstable and changes between nightlies. A
/// different one still parses, but the surface it yields can differ, and a
/// derived artifact generated under one version will look "stale" under
/// another. Naming both numbers turns that into an explicable message.
pub(crate) const RUSTDOC_FORMAT_VERSION: u64 = 61;

/// Toolchain that builds rustdoc JSON, `SOOTHFAST_RUSTDOC_TOOLCHAIN` or
/// plain `nightly`.
///
/// Pin a dated nightly to keep committed artifacts (generated
/// specs) reproducible: on a floating `nightly` they are only valid until
/// rustdoc next changes its output.
fn rustdoc_toolchain() -> String {
    std::env::var("SOOTHFAST_RUSTDOC_TOOLCHAIN").unwrap_or_else(|_| "nightly".to_string())
}

/// Warn when rustdoc's JSON is not the version this build knows, once per
/// process: the same mismatch would otherwise repeat per documented crate.
fn warn_on_format_version(doc: &Value) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);

    let Some(found) = doc["format_version"].as_u64() else {
        return;
    };
    if found == RUSTDOC_FORMAT_VERSION || WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!(
        "warning: rustdoc JSON format_version {found}, expected {RUSTDOC_FORMAT_VERSION}. \
         The extracted surface may differ from the one a committed artifact was \
         generated with. Pin a nightly with SOOTHFAST_RUSTDOC_TOOLCHAIN to make it \
         reproducible."
    );
}

/// `pkg`'s own crate directory plus every workspace-local (path) dependency
/// it depends on, transitively — the source trees whose changes can alter
/// `pkg`'s rustdoc JSON without touching `pkg`'s own files (e.g. through a
/// re-exported type).
fn local_crate_dirs(pkg: &str, dir: Option<&Path>) -> io::Result<Vec<PathBuf>> {
    let mut cmd = Command::new("cargo");
    cmd.args(["metadata", "--format-version", "1"]);
    cmd.stdout(Stdio::piped());
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(io::Error::other("cargo metadata failed"));
    }
    let meta: Value = serde_json::from_slice(&out.stdout)?;
    let packages = meta["packages"].as_array().cloned().unwrap_or_default();

    let mut dir_of: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut local: BTreeSet<String> = BTreeSet::new();
    for p in &packages {
        let Some(id) = p["id"].as_str() else { continue };
        if let Some(cd) = p["manifest_path"]
            .as_str()
            .and_then(|m| Path::new(m).parent())
        {
            dir_of.insert(id.to_string(), cd.to_path_buf());
        }
        if p["source"].is_null() {
            local.insert(id.to_string());
        }
    }
    let Some(root_id) = packages
        .iter()
        .find(|p| p["name"] == pkg)
        .and_then(|p| p["id"].as_str())
        .map(str::to_string)
    else {
        return Ok(Vec::new());
    };
    let deps_of: BTreeMap<String, Vec<String>> = meta["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            let id = n["id"].as_str()?.to_string();
            let deps = n["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|d| d.as_str().map(str::to_string))
                .collect();
            Some((id, deps))
        })
        .collect();

    let mut seen = BTreeSet::new();
    let mut stack = vec![root_id];
    let mut dirs = Vec::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if local.contains(&id)
            && let Some(d) = dir_of.get(&id)
        {
            dirs.push(d.clone());
        }
        stack.extend(deps_of.get(&id).cloned().into_iter().flatten());
    }
    Ok(dirs)
}

/// Build rustdoc JSON for a package (nightly-only job) and load it.
/// `dir` runs the build in another worktree; JSON lands in its target/doc.
///
/// Reuses a stored extraction of the same inputs (see `rustdocstore`) instead
/// of re-running `cargo rustdoc`: rustdoc's JSON backend re-serializes on every
/// invocation even when the compilation is fully cached, so repeat calls for
/// one package, and the same sources in another run, would pay for it again.
pub fn rustdoc_json_in(
    pkg: &str,
    dir: Option<&Path>,
    features: Option<&str>,
    visibility: Visibility,
) -> io::Result<(Value, PathBuf)> {
    let toolchain = rustdoc_toolchain();
    let root = match dir {
        Some(d) => d.to_path_buf(),
        None => workspace_root()?,
    };
    // Honor CARGO_TARGET_DIR / build.target-dir: a hardcoded target/ would
    // silently read a STALE json and validate docs against an old surface.
    let json_path = target_dir(dir)?
        .join("doc")
        .join(format!("{}.json", pkg.replace('-', "_")));

    let store_key = || rustdoc_store_key(pkg, dir, &root, features, visibility, &toolchain);
    let key = store_key();
    if let Some(doc) = key.as_deref().and_then(|k| rustdocstore::load(k, &root)) {
        warn_on_format_version(&doc);
        return Ok((doc, root));
    }
    let dep_info = json_path.with_extension("soothfast.d");
    let _ = std::fs::remove_file(&dep_info);
    let started = std::time::SystemTime::now();

    let build = |dep_info: Option<&Path>| {
        let cmd = rustdoc_command(&toolchain, pkg, dir, features, visibility, dep_info);
        timed(&format!("cargo rustdoc {pkg}"), || run_teeing_stderr(cmd))
    };
    let mut built = build(Some(&dep_info))?;
    let mut tracked = true;
    if !built.status.success() && rejects_tracking_flags(&built.stderr) {
        tracked = false;
        built = build(None)?;
    }
    let failed = |built: &Built| {
        let hint = match toolchain_trouble(&built.stderr, &toolchain) {
            Some(ToolchainTrouble::Missing) => {
                format!(" (is the {toolchain} toolchain installed?)")
            }
            Some(ToolchainTrouble::Components) => {
                format!(" ({toolchain} could not be installed with its components)")
            }
            None => String::new(),
        };
        io::Error::other(format!(
            "cargo +{toolchain} rustdoc failed with {}{hint}",
            built.status
        ))
    };
    if !built.status.success() {
        return Err(failed(&built));
    }
    // With `--emit=dep-info` rustdoc writes nothing else, so the JSON comes
    // from a second run without it.
    if tracked {
        let plain = build(None)?;
        if !plain.status.success() {
            return Err(failed(&plain));
        }
    }
    let text = std::fs::read_to_string(&json_path)
        .map_err(|e| io::Error::other(format!("cannot read {}: {e}", json_path.display())))?;
    let doc: Value = timed(&format!("parse rustdoc json {pkg}"), || {
        serde_json::from_str(&text)
    })?;
    warn_on_format_version(&doc);
    if let (Some(key), true) = (key, tracked) {
        let inputs = rustdoc_inputs::collect(&root, &dep_info, &built.stdout);
        rustdocstore::store(&key, store_key(), &root, &inputs, started, &json_path);
    }
    Ok((doc, root))
}

/// The store key `rustdoc_json_in` looks `pkg` up under in the workspace.
pub fn rustdoc_key(pkg: &str, features: Option<&str>, visibility: Visibility) -> Option<String> {
    let root = workspace_root().ok()?;
    rustdoc_store_key(pkg, None, &root, features, visibility, &rustdoc_toolchain())
}

/// Whether the store holds the rustdoc JSON under `key`.
pub fn rustdoc_holds(key: &str) -> bool {
    workspace_root().is_ok_and(|root| rustdocstore::holds(key, &root))
}

/// `cargo rustdoc` for `pkg`, recording what it read in `dep_info` when given.
fn rustdoc_command(
    toolchain: &str,
    pkg: &str,
    dir: Option<&Path>,
    features: Option<&str>,
    visibility: Visibility,
    dep_info: Option<&Path>,
) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.args([&format!("+{toolchain}"), "rustdoc", "-p", pkg, "--lib"]);
    if dep_info.is_some() {
        cmd.arg("--message-format=json-render-diagnostics");
    }
    // Feature-gated items are invisible to the surface unless enabled.
    if let Some(f) = features {
        cmd.args(["--features", f]);
    }
    cmd.args(["--", "--output-format", "json", "-Zunstable-options"]);
    if let Some(path) = dep_info {
        cmd.arg(format!("--emit=dep-info={}", path.display()));
    }
    if visibility == Visibility::Private {
        cmd.arg("--document-private-items");
    }
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    cmd
}

/// Whether `stderr` says the toolchain refused the flags that record what a
/// rustdoc build read, as opposed to the documented crate failing to build.
fn rejects_tracking_flags(stderr: &str) -> bool {
    let e = stderr.to_lowercase();
    e.contains("invalid message format")
        || (e.contains("emit")
            && (e.contains("unrecognized option") || e.contains("unexpected argument")))
}

/// Why a `cargo +TOOLCHAIN` failure is the toolchain's fault rather than the
/// documented crate's.
#[derive(Debug, PartialEq)]
enum ToolchainTrouble {
    /// Absent, or rustup could not fetch it.
    Missing,
    /// rustup has the toolchain but not every component it must install with it.
    Components,
}

/// What a `cargo +TOOLCHAIN` failure says is wrong with `toolchain`, from
/// rustup or from a `cargo` that is no rustup proxy. A component missing from
/// an installed toolchain is not its fault, and neither is a build script's
/// error, whether cargo replays it under `--- stderr` or as `error: PKG@VER:`.
fn toolchain_trouble(stderr: &str, toolchain: &str) -> Option<ToolchainTrouble> {
    let tc = toolchain.to_lowercase();
    let channel = tc.split_once('-').map_or(tc.as_str(), |(c, _)| c);
    let quoted = format!("'{tc}");
    let dist = format!("channel-rust-{channel}");
    let unavailable = format!("is unavailable for download for channel {quoted}");
    let no_release = format!("no release found for {quoted}");
    let no_command = format!("no such command: `+{tc}");
    let fetch = [
        "could not download",
        "failed to install",
        "error downloading",
    ];
    let from_build_script = |l: &str| {
        l.strip_prefix("error:")
            .and_then(|rest| rest.split_whitespace().next())
            .is_some_and(|word| word.ends_with(':') && word.contains('@'))
    };
    let named_missing = |l: &str| {
        !l.contains("component")
            && l.find(&quoted).is_some_and(|at| {
                l[at + quoted.len()..]
                    .split_once(' ')
                    .is_some_and(|(word, after)| {
                        word.ends_with('\'') && after.starts_with("is not install")
                    })
            })
    };
    let mut lines = stderr
        .lines()
        .map(|l| l.trim().to_lowercase())
        .take_while(|l| !l.starts_with("--- stderr") && !l.starts_with("--- stdout"))
        .filter(|l| l.starts_with("error:") && !from_build_script(l));
    lines.find_map(|l| {
        if l.contains(&unavailable) {
            return Some(ToolchainTrouble::Components);
        }
        let fetch_failed =
            fetch.iter().any(|p| l.contains(p)) && (l.contains(&quoted) || l.contains(&dist));
        (named_missing(&l) || fetch_failed || l.contains(&no_release) || l.contains(&no_command))
            .then_some(ToolchainTrouble::Missing)
    })
}

struct Built {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: String,
}

/// Run `cmd`, passing its stderr through as it arrives and keeping a copy.
fn run_teeing_stderr(mut cmd: Command) -> io::Result<Built> {
    use std::io::{Read, Write};
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut err = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("no stderr"))?;
    let tee = std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut chunk = [0u8; 8192];
        while let Ok(n) = err.read(&mut chunk) {
            if n == 0 {
                break;
            }
            let _ = io::stderr().write_all(&chunk[..n]);
            kept.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8_lossy(&kept).into_owned()
    });
    let out = child.wait_with_output()?;
    let stderr = tee.join().unwrap_or_default();
    Ok(Built {
        status: out.status,
        stdout: out.stdout,
        stderr,
    })
}

/// `local_crate_dirs`, resolved once per package and tree.
pub(crate) fn crate_dirs(pkg: &str, dir: Option<&Path>) -> Option<Vec<PathBuf>> {
    type Dirs = BTreeMap<(String, Option<PathBuf>), Vec<PathBuf>>;
    static DIRS: Mutex<Dirs> = Mutex::new(BTreeMap::new());
    let at = (pkg.to_string(), dir.map(Path::to_path_buf));
    let known = DIRS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&at)
        .cloned();
    if known.is_some() {
        return known;
    }
    let dirs = local_crate_dirs(pkg, dir).ok()?;
    DIRS.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(at, dirs.clone());
    Some(dirs)
}

fn rustdoc_store_key(
    pkg: &str,
    dir: Option<&Path>,
    root: &Path,
    features: Option<&str>,
    visibility: Visibility,
    toolchain: &str,
) -> Option<String> {
    let crate_dirs = crate_dirs(pkg, dir)?;
    let rustc = store::rustc_version(Some(toolchain), root)?;
    let env = store::build_env(&[]);
    let request = rustdocstore::Request {
        pkg,
        features,
        visibility,
        rustc: &rustc,
        env: &env,
    };
    rustdocstore::key(root, &crate_dirs, &request)
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_toolchain_that_refuses_the_tracking_flags_is_told_from_a_failed_build() {
        let refused = [
            "error: Unrecognized option: 'emit'",
            "error: invalid message format specifier: `json-render-diagnostics`",
            "error: unexpected argument '--emit' found",
        ];
        for stderr in refused {
            assert!(super::rejects_tracking_flags(stderr), "{stderr}");
        }
        let failed = "error[E0425]: cannot find value `x` in this scope\n\
                      Caused by: process didn't exit successfully: `rustdoc --emit=dep-info=x.d`";
        assert!(!super::rejects_tracking_flags(failed));
    }

    #[test]
    fn only_a_missing_toolchain_is_blamed_on_the_toolchain() {
        use super::ToolchainTrouble::{Components, Missing};
        let tc = "nightly-2026-08-07";
        let blamed = [
            (
                "error: toolchain 'nightly-2026-08-07-x86_64-unknown-linux-gnu' is not installed",
                Missing,
            ),
            (
                "error: 'nightly-2026-08-07' is not installed\nhelp: run `rustup toolchain install`",
                Missing,
            ),
            (
                "error: toolchain 'nightly-2026-08-07' is not installable",
                Missing,
            ),
            ("error: no release found for 'nightly-2026-08-07'", Missing),
            ("error: no such command: `+nightly-2026-08-07`", Missing),
            (
                "info: syncing channel updates for 'nightly-2026-08-07'\n\
                 error: could not download file from \
                 'https://static.rust-lang.org/dist/2026-08-07/channel-rust-nightly.toml.sha256'",
                Missing,
            ),
            (
                "error: failed to install toolchain 'nightly-2026-08-07'",
                Missing,
            ),
            (
                "error: component 'rust-docs-json' for target 'x86_64-unknown-linux-gnu' \
                 is unavailable for download for channel 'nightly-2026-08-07'",
                Components,
            ),
        ];
        for (stderr, trouble) in blamed {
            assert_eq!(
                super::toolchain_trouble(stderr, tc),
                Some(trouble),
                "{stderr}"
            );
        }
        let elsewhere = [
            "error: no library targets found in package `tool`",
            "error: 'rustdoc' is not installed for the toolchain 'nightly-2026-08-07'",
            "error: 'cargo-clippy' is not installed for the toolchain \
             'nightly-2026-08-07-x86_64-unknown-linux-gnu'",
            "error: component download failed for rust-docs-x86_64-unknown-linux-gnu",
            "error: toolchain 'stable' is not installed",
            "error: failed to run custom build command for `foo-sys v0.1.0`\n\
             --- stderr\n  error: error downloading libfoo for 'nightly-2026-08-07'",
            "error: foo-sys@0.1.0: failed to install libfoo for 'nightly-2026-08-07'",
        ];
        for stderr in elsewhere {
            assert_eq!(super::toolchain_trouble(stderr, tc), None, "{stderr}");
        }
    }

    #[test]
    fn a_failing_command_keeps_its_stderr_and_stdout() {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", "echo out; echo oops >&2; exit 3"]);
        let built = super::run_teeing_stderr(cmd).unwrap();
        assert_eq!(built.status.code(), Some(3));
        assert_eq!(built.stdout, b"out\n");
        assert_eq!(built.stderr, "oops\n");
    }
}
