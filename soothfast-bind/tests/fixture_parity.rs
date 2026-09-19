//! Walks a real nightly's rustdoc JSON for `tests/fixture_crate` and checks
//! it against the synthetic document `tests/fixture/mod.rs` hand-writes:
//! every other test in this crate trusts that synthetic shape without ever
//! comparing it to what rustdoc actually emits.
//!
//! Ignored by default: it needs the pinned nightly rustdoc toolchain
//! (`SOOTHFAST_RUSTDOC_TOOLCHAIN`, `nightly` if unset, the same default
//! `cargo-soothfast` itself falls back to). Run with:
//! `cargo test -p soothfast-bind --test fixture_parity -- --ignored`

mod fixture;

use std::path::{Path, PathBuf};
use std::process::Command;

use soothfast_bind::foreign::TypeTable;
use soothfast_bind::walk::surface;

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).into()
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("makes a directory");
    for entry in std::fs::read_dir(src).expect("reads the fixture crate") {
        let entry = entry.expect("reads a directory entry");
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copies a fixture file");
        }
    }
}

fn rustdoc_toolchain() -> String {
    std::env::var("SOOTHFAST_RUSTDOC_TOOLCHAIN").unwrap_or_else(|_| "nightly".to_string())
}

/// Real rustdoc JSON for `tests/fixture_crate`, built in a scratch copy the
/// same way every smoke test isolates its build (fixture_crate sits inside
/// this workspace, so `cargo rustdoc` in place would fight the outer
/// workspace's own `[workspace]` resolution).
fn real_doc() -> serde_json::Value {
    let scratch =
        std::env::temp_dir().join(format!("soothfast-fixture-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    copy_dir(&manifest_dir().join("tests/fixture_crate"), &scratch);

    let toolchain = rustdoc_toolchain();
    let status = Command::new("rustup")
        .args(["run", &toolchain, "cargo", "rustdoc", "-p", "acme", "--lib"])
        .arg("--")
        .args(["--output-format", "json", "-Zunstable-options"])
        .arg("--document-private-items")
        .current_dir(&scratch)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .unwrap_or_else(|e| panic!("cannot run cargo +{toolchain} rustdoc: {e}"));
    assert!(status.success(), "cargo +{toolchain} rustdoc failed");

    let json_path = scratch.join("target/doc/acme.json");
    let text = std::fs::read_to_string(&json_path)
        .unwrap_or_else(|e| panic!("reads {}: {e}", json_path.display()));
    let doc = serde_json::from_str(&text).expect("parses rustdoc JSON");

    let _ = std::fs::remove_dir_all(&scratch);
    doc
}

#[test]
#[ignore = "needs the pinned nightly rustdoc toolchain; run with --ignored"]
fn walking_real_rustdoc_matches_the_synthetic_fixture() {
    // `with_time` takes `chrono::DateTime`, and adding a real dependency to
    // fixture_crate just to reproduce one already-covered foreign-type gap
    // (see tests/surface.rs) would slow every other smoke's build for no
    // parity gained.
    let records: Vec<_> = fixture::records()
        .into_iter()
        .filter(|r| r.id != "acme::with_time")
        .collect();

    let doc = real_doc();
    let (real_surface, real_gaps) =
        surface(&doc, &TypeTable::with_defaults(), &records).expect("walks the real doc");

    let (mut synthetic_surface, mut synthetic_gaps) = fixture::walk();
    synthetic_surface.fns.retain(|f| f.id != "acme::with_time");
    synthetic_gaps.retain(|g| g.at() != "acme::with_time");

    assert_eq!(
        real_surface, synthetic_surface,
        "the real rustdoc surface no longer matches tests/fixture/mod.rs's synthetic one"
    );
    assert_eq!(
        real_gaps, synthetic_gaps,
        "the real rustdoc gaps no longer match tests/fixture/mod.rs's synthetic ones"
    );
}
