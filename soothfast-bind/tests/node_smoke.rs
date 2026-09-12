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

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).into()
}

fn golden_dir() -> PathBuf {
    manifest_dir().join("tests/goldens/node")
}

fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!("soothfast-node-smoke-{}", std::process::id()))
}

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

fn copy_dir(src: &Path, dest: &Path) {
    fs::create_dir_all(dest).expect("makes dest dir");
    for entry in fs::read_dir(src).expect("reads dir") {
        let entry = entry.expect("reads entry");
        let path = entry.path();
        let target = dest.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            fs::copy(&path, &target).expect("copies file");
        }
    }
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
    copy_dir(&manifest_dir().join("tests/fixture_crate"), &scratch);
    let glue = scratch.join("node");
    copy_dir(&golden_dir(), &glue);

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
