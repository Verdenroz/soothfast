//! End-to-end: build the Go golden's cdylib for real, then run `go test`
//! against it through the actual cgo linker path.
//!
//! Ignored by default since it shells out to `cargo build --release` and
//! `go test`. Run with:
//! `cargo test -p soothfast-bind --test go_smoke -- --ignored`
//!
//! The golden's `Cargo.toml` depends on `acme` at `path = ".."`, so this
//! copies `tests/fixture_crate` one directory above the copied golden, the
//! same layout `bind gen` produces for a real package.

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).into()
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("makes a directory");
    for entry in std::fs::read_dir(src).expect("reads the golden") {
        let entry = entry.expect("reads a directory entry");
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copies a golden file");
        }
    }
}

#[test]
#[ignore]
fn the_go_golden_builds_and_runs_against_the_real_cdylib() {
    let manifest = manifest_dir();
    let root = std::env::temp_dir().join(format!("soothfast-go-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    copy_dir(&manifest.join("tests/fixture_crate"), &root);

    let glue = root.join("glue");
    copy_dir(&manifest.join("tests/goldens/go"), &glue);
    std::fs::copy(
        manifest.join("tests/go/Smoke_test.go"),
        glue.join("Smoke_test.go"),
    )
    .expect("copies the smoke test");

    // The glue crate's LDFLAGS point at its own `target/release`, so the
    // build must land there rather than wherever the outer test run points
    // CARGO_TARGET_DIR.
    let build = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&glue)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("runs cargo build");
    assert!(build.success(), "cargo build --release failed");

    let test = Command::new("go")
        .args(["test", "./..."])
        .current_dir(&glue)
        .env("CGO_ENABLED", "1")
        .status()
        .expect("runs go test");
    assert!(test.success(), "go test failed");

    let _ = std::fs::remove_dir_all(&root);
}
