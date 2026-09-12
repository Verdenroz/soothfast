//! End-to-end: build the Go golden's cdylib for real, then run `go test`
//! against it through the actual cgo linker path.
//!
//! Ignored by default since it shells out to `cargo build --release` and
//! `go test`. Run with:
//! `cargo test -p soothfast-bind --test go_smoke -- --ignored`
//!
//! The golden's `Cargo.toml` depends on `acme` at `path = ".."`, so this
//! writes a real crate implementing that surface one directory above the
//! copied golden, the same layout `bind gen` produces for a real package.

use std::path::Path;
use std::process::Command;

const ACME_CARGO_TOML: &str = "[package]
name = \"acme\"
version = \"0.1.0\"
edition = \"2024\"

[workspace]
";

const ACME_SRC: &str = "
pub struct Counter {
    pub value: i64,
}

impl Counter {
    pub fn new(start: i64) -> Self {
        Counter { value: start }
    }

    pub fn bump(&self, by: i64) -> Result<i64, String> {
        if by < 0 {
            return Err(\"cannot bump by a negative amount\".to_string());
        }
        Ok(self.value + by)
    }

    pub fn bump_all(&self, by: Vec<i64>) -> i64 {
        self.value + by.iter().sum::<i64>()
    }

    pub fn at(&self, level: Level) -> i64 {
        match level {
            Level::Low => self.value,
            Level::High => self.value * 2,
        }
    }
}

#[derive(Clone, Copy)]
pub enum Level {
    Low,
    High,
}

pub enum Mode {
    Fast,
    Precise(u32),
    Custom { level: u8 },
}

pub fn digest(data: &[u8]) -> Vec<u8> {
    data.iter().rev().copied().collect()
}

pub fn normalize(input: Vec<f64>, factor: f64) -> Vec<f64> {
    input.into_iter().map(|v| v * factor).collect()
}

pub fn stamp(handle: i64, error: f64, register: &[u8]) -> u64 {
    handle as u64 + error as u64 + register.len() as u64
}
";

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
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = std::env::temp_dir().join(format!("soothfast-go-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    std::fs::create_dir_all(root.join("src")).expect("makes the acme crate dir");
    std::fs::write(root.join("Cargo.toml"), ACME_CARGO_TOML).expect("writes acme's Cargo.toml");
    std::fs::write(root.join("src/lib.rs"), ACME_SRC).expect("writes acme's lib.rs");

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
