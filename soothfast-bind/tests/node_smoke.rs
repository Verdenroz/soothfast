//! Builds the Node golden into a real native addon and exercises it.
//!
//! Ignored by default: it needs `npm` reachable and a working `cargo`/napi
//! toolchain to build a `.node` file, neither of which the rest of the
//! suite requires. Run with:
//!
//! ```text
//! cargo test -p soothfast-bind --test node_smoke -- --ignored
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/node")
}

fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!("soothfast-node-smoke-{}", std::process::id()))
}

/// A real `acme` crate shaped like the synthetic surface the golden was
/// generated from, so the generated glue has something to actually link
/// against and run.
const ACME_SRC: &str = r#"
pub struct Counter {
    pub value: i64,
}

impl Counter {
    pub fn new(start: i64) -> Self {
        Counter { value: start }
    }

    pub fn bump(&self, by: i64) -> Result<i64, String> {
        self.value
            .checked_add(by)
            .ok_or_else(|| "overflow".to_string())
    }

    pub fn bump_all(&self, by: Vec<i64>) -> i64 {
        by.into_iter().fold(self.value, |acc, x| acc + x)
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

pub enum Mode {}

pub fn digest(data: &[u8]) -> Vec<u8> {
    data.iter().map(|b| b.wrapping_add(1)).collect()
}

pub fn normalize(input: Vec<f64>, factor: f64) -> Vec<f64> {
    input.into_iter().map(|v| v * factor).collect()
}

pub fn stamp(handle: i64, error: f64, register: &[u8]) -> u64 {
    (handle as u64) ^ (error as u64) ^ (register.len() as u64)
}

pub fn trim(input: &[f64]) -> Option<Vec<f64>> {
    match input.is_empty() {
        true => None,
        false => Some(input.to_vec()),
    }
}
"#;

const SMOKE_TEST_JS: &str = r#"
const test = require("node:test");
const assert = require("node:assert");
const { Counter, digest, normalize } = require("./index.js");

test("a class carries state across calls", () => {
    const counter = new Counter(10n);
    assert.strictEqual(counter.value, 10n);
    assert.strictEqual(counter.bump(5n), 15n);
    assert.strictEqual(counter.bump(1n), 11n);
});

test("a buffer call reaches Rust without boxing every byte", () => {
    const bytes = digest(Buffer.from([1, 2, 3]));
    assert.deepStrictEqual(Buffer.from(bytes), Buffer.from([2, 3, 4]));
    const scaled = normalize(Float64Array.from([1, 2, 3]), 2);
    assert.deepStrictEqual(Array.from(scaled), [2, 4, 6]);
});

test("a failing call throws a JavaScript Error", () => {
    const counter = new Counter(9223372036854775807n);
    assert.throws(() => counter.bump(1n));
});

test("a BigInt outside i64 range is rejected rather than truncated", () => {
    assert.throws(() => new Counter(2n ** 100n));
});
"#;

fn write_acme_crate(dir: &Path) {
    fs::create_dir_all(dir.join("src")).expect("makes acme dirs");
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"acme\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("writes acme Cargo.toml");
    fs::write(dir.join("src/lib.rs"), ACME_SRC).expect("writes acme src");
}

fn copy_golden(dest: &Path) {
    fn walk(src: &Path, dest: &Path) {
        fs::create_dir_all(dest).expect("makes dest dir");
        for entry in fs::read_dir(src).expect("reads golden dir") {
            let entry = entry.expect("reads entry");
            let path = entry.path();
            let target = dest.join(entry.file_name());
            if path.is_dir() {
                walk(&path, &target);
            } else {
                fs::copy(&path, &target).expect("copies golden file");
            }
        }
    }
    walk(&golden_dir(), dest);
}

fn run(cmd: &mut Command, what: &str) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("cannot run {what}: {e}"));
    assert!(status.success(), "{what} failed");
}

#[test]
#[ignore = "builds a real native addon; run with --ignored"]
fn the_node_golden_builds_and_runs() {
    let scratch = scratch_dir();
    let _ = fs::remove_dir_all(&scratch);
    // The glue's Cargo.toml names its dependency by `crate_path = ".."`, so
    // the acme crate has to sit at the golden's own parent directory.
    write_acme_crate(&scratch);
    let glue = scratch.join("node");
    copy_golden(&glue);

    run(
        Command::new("npm").arg("install").current_dir(&glue),
        "npm install",
    );
    run(
        Command::new("npx")
            .args(["napi", "build", "--platform", "--release"])
            .current_dir(&glue),
        "napi build --platform --release",
    );

    let script = glue.join("smoke.test.js");
    fs::write(&script, SMOKE_TEST_JS).expect("writes smoke script");
    run(
        Command::new("node")
            .arg("--test")
            .arg(&script)
            .current_dir(&glue),
        "node --test",
    );

    let _ = fs::remove_dir_all(&scratch);
}
