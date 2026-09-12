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

#[test]
#[ignore = "compiles every committed binding package; run with --ignored"]
fn every_committed_glue_compiles() {
    let mut checked = Vec::new();
    let mut failed = Vec::new();

    for entry in fs::read_dir(bindings_dir()).expect("reads bindings dir") {
        let path = entry.expect("reads entry").path();
        if !path.join("Cargo.toml").is_file() {
            continue;
        }
        let name = path
            .file_name()
            .expect("has a name")
            .to_string_lossy()
            .into_owned();

        // Each package is its own self-contained workspace, built where its
        // own Cargo.toml expects rather than wherever the outer test run
        // points CARGO_TARGET_DIR.
        let status = Command::new("cargo")
            .args(["check", "--release"])
            .current_dir(&path)
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
}
