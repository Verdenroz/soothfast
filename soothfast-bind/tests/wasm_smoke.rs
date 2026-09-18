//! Builds the wasm golden with `wasm-pack build --target nodejs` and runs
//! it from a real `node`, the same shape `node_smoke.rs` proves for napi.
//!
//! Ignored by default: it needs `wasm-pack` and `node` reachable, and the
//! `wasm32-unknown-unknown` target installed. Run with:
//! `cargo test -p soothfast-bind --test wasm_smoke -- --ignored`

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).into()
}

fn golden_dir() -> PathBuf {
    manifest_dir().join("tests/goldens/wasm")
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

fn run(cmd: &mut Command, what: &str) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("cannot run {what}: {e}"));
    assert!(status.success(), "{what} failed");
}

const SMOKE_TEST_JS: &str = r#"
const test = require("node:test");
const assert = require("node:assert");
const { Counter, Level, digest, normalize, greet, peakLevel, findCounter, describe, describeOwned, maybeRatio } = require("./pkg");

test("a class carries state across calls", () => {
    const counter = new Counter(10n);
    assert.strictEqual(counter.value, 10n);
    assert.strictEqual(counter.at(Level.Low), 10n);
    assert.strictEqual(counter.at(Level.High), 20n);
    assert.strictEqual(counter.bump(5n), 15n);
    assert.strictEqual(counter.bump(1n), 11n);
    assert.strictEqual(counter.bumpAll([1n, 2n, 3n]), 16n);
});

test("an async method resolves through the wasm runtime", async () => {
    const counter = new Counter(10n);
    assert.strictEqual(await counter.refresh(), 10);
});

test("a failing call throws the error string", () => {
    const counter = new Counter(9223372036854775807n);
    assert.throws(() => counter.bump(1n), /overflow/);
});

test("byte and float sequences cross as plain arrays", () => {
    const bytes = digest(Uint8Array.from([1, 2, 3]));
    assert.deepStrictEqual(Array.from(bytes), [2, 3, 4]);
    const scaled = normalize([1, 2, 3], 2);
    assert.deepStrictEqual(Array.from(scaled), [2, 4, 6]);
});

test("greet round-trips a plain string", () => {
    assert.strictEqual(greet("wasm"), "hello, wasm");
});

test("a mirrored enum returns by value and an optional handle maps into its wrapper", () => {
    assert.strictEqual(peakLevel([0.1, 0.9, 0.3]), Level.High);
    assert.strictEqual(peakLevel([0.1, 0.2]), Level.Low);

    const found = findCounter(5n);
    assert.notStrictEqual(found, undefined);
    assert.strictEqual(found.value, 5n);

    assert.strictEqual(findCounter(-1n), undefined);
});

test("an optional string round-trips through Option<&str> and Option<String>", () => {
    assert.strictEqual(describe("world"), "label=world");
    assert.strictEqual(describe(undefined), undefined);

    assert.strictEqual(describeOwned("world"), "owned=world");
    assert.strictEqual(describeOwned(undefined), undefined);
});

test("an optional scalar round-trips through Option<f64>", () => {
    assert.strictEqual(maybeRatio(4), 0.25);
    assert.strictEqual(maybeRatio(-1), undefined);
});
"#;

#[test]
#[ignore = "builds a real wasm package; run with --ignored"]
fn the_wasm_golden_builds_and_runs() {
    if !support::require_toolchain(
        Command::new("wasm-pack").arg("--version").output().is_ok(),
        "wasm-pack",
        "cargo install wasm-pack --locked",
    ) {
        return;
    }

    let scratch = std::env::temp_dir().join(format!("soothfast-wasm-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    // The glue's Cargo.toml names its dependency by `crate_path = ".."`, so
    // the acme crate has to sit at the golden's own parent directory.
    copy_dir(&manifest_dir().join("tests/fixture_crate"), &scratch);
    let glue = scratch.join("wasm");
    copy_dir(&golden_dir(), &glue);

    run(
        Command::new("cargo")
            .arg("generate-lockfile")
            .current_dir(&glue)
            .env_remove("CARGO_TARGET_DIR"),
        "cargo generate-lockfile",
    );
    let mut wasm_pack = Command::new("wasm-pack");
    wasm_pack
        .args(["build", "--target", "nodejs", "--", "--locked"])
        .current_dir(&glue)
        .env_remove("CARGO_TARGET_DIR");
    // `[build] rustflags` still reaches the wasm32 link and an empty target
    // override reads as unset, so a no-op flag stands in.
    if std::env::var_os("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS").is_none() {
        wasm_pack.env(
            "CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS",
            "-Cstrip=none",
        );
    }
    run(&mut wasm_pack, "wasm-pack build --target nodejs");

    let script = glue.join("smoke.test.js");
    std::fs::write(&script, SMOKE_TEST_JS).expect("writes smoke script");
    run(
        Command::new("node")
            .arg("--test")
            .arg(&script)
            .current_dir(&glue),
        "node --test",
    );

    let _ = std::fs::remove_dir_all(&scratch);
}
