//! Compiles every committed binding package for real.
//!
//! `bind gen --check` only proves the glue's *text* matches the plan; it
//! never proves the text still compiles against the crate it wraps. This is
//! the check that would have caught the Node glue calling `Summary::parse`
//! with an owned `String` where it wanted `&str`.
//!
//! Ignored by default: each package pulls its own toolchain (napi needs
//! `cargo` alone, but building it is still real compile work every other
//! test in this crate does not need). Run with:
//!
//! ```text
//! cargo test -p soothfast-demo --test glue_compiles -- --ignored
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bindings_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("bindings")
}

/// The manifest for a top-level package directory, wherever the emitter put
/// it (`r` and `ruby` nest theirs a few levels down, under `src/rust` and
/// `ext/<crate>`). Depth-limited and skipping `target`/`node_modules` so it
/// can't wander into a build output from an earlier run.
fn find_manifest_dir(dir: &Path, depth: u32) -> Option<PathBuf> {
    if dir.join("Cargo.toml").is_file() {
        return Some(dir.to_path_buf());
    }
    if depth == 0 {
        return None;
    }
    for entry in fs::read_dir(dir).expect("reads directory") {
        let path = entry.expect("reads entry").path();
        let name = path.file_name().expect("has a name").to_string_lossy();
        if !path.is_dir() || name == "target" || name == "node_modules" {
            continue;
        }
        if let Some(found) = find_manifest_dir(&path, depth - 1) {
            return Some(found);
        }
    }
    None
}

/// `Some(reason)` when `pkg` needs a toolchain this machine doesn't have.
///
/// Mirrors the detection `soothfast-bind`'s `ruby_smoke` test uses: `rb_sys`
/// needs Ruby's own headers, which `cargo check` cannot get without `ruby`
/// on PATH, and that absence is this machine's normal state, not a failure.
fn missing_toolchain(pkg: &str) -> Option<String> {
    if pkg != "ruby" {
        return None;
    }
    match Command::new("ruby").arg("--version").output() {
        Ok(_) => None,
        Err(_) => Some("`ruby` not found on PATH".to_string()),
    }
}

#[test]
#[ignore = "compiles every committed binding package; run with --ignored"]
fn every_committed_glue_compiles() {
    let mut checked = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = Vec::new();

    for entry in fs::read_dir(bindings_dir()).expect("reads bindings dir") {
        let top = entry.expect("reads entry").path();
        if !top.is_dir() {
            continue;
        }
        let name = top
            .file_name()
            .expect("has a name")
            .to_string_lossy()
            .into_owned();

        if let Some(reason) = missing_toolchain(&name) {
            println!("{name}: skipped ({reason})");
            skipped.push(name);
            continue;
        }

        let manifest_dir = find_manifest_dir(&top, 4)
            .unwrap_or_else(|| panic!("{name}: no Cargo.toml found under its directory"));

        // Each package is its own self-contained workspace, built where its
        // own Cargo.toml expects rather than wherever the outer test run
        // points CARGO_TARGET_DIR.
        let status = Command::new("cargo")
            .args(["check", "--release"])
            .current_dir(&manifest_dir)
            .env_remove("CARGO_TARGET_DIR")
            .status()
            .unwrap_or_else(|e| panic!("cannot run cargo check in {name}: {e}"));

        println!("{name}: {}", if status.success() { "ok" } else { "FAILED" });
        checked.push(name.clone());
        if !status.success() {
            failed.push(name);
        }
    }

    assert!(!checked.is_empty(), "no binding package found to check");
    assert!(failed.is_empty(), "glue failed to compile: {failed:?}");
    if !skipped.is_empty() {
        println!("skipped (missing toolchain): {skipped:?}");
    }
}
