//! Builds the Python golden with `maturin` into a scratch venv (mirroring
//! `bind bench`'s own launcher) and runs a real interpreter against it.
//!
//! Ignored by default: it needs `maturin` and a `python3` reachable, neither
//! of which the rest of the suite requires. Run with:
//! `cargo test -p soothfast-bind --test python_smoke -- --ignored`

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).into()
}

fn golden_dir() -> PathBuf {
    manifest_dir().join("tests/goldens/python")
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

const SMOKE_SCRIPT: &str = r#"
import array
import asyncio

import acme_core
from acme_core import Counter, Level

c = Counter(10)
assert c.value == 10
assert repr(c) == "Counter(value=10)"
assert c.bump(5) == 15
assert c.bump(1) == 11
assert c.bump_all([1, 2, 3]) == 16
assert c.at(Level.Low) == 10
assert c.at(Level.High) == 20

try:
    Counter(9223372036854775807).bump(1)
    raise AssertionError("expected an error")
except acme_core.Error as e:
    assert "overflow" in str(e)
assert issubclass(acme_core.Error, Exception)
assert not issubclass(acme_core.Error, RuntimeError)

try:
    Counter(2**100)
    raise AssertionError("expected an OverflowError")
except OverflowError:
    pass

assert asyncio.run(c.refresh()) == 10
assert c.refresh_blocking() == 10

assert bytes(acme_core.digest(bytes([1, 2, 3]))) == bytes([2, 3, 4])

normalized = acme_core.normalize([1.0, 2.0, 3.0], 2.0)
assert normalized.tolist() == [2.0, 4.0, 6.0]
# A returned F64Array passes back into another call through the buffer
# protocol, with no list round trip in between.
renormalized = acme_core.normalize(normalized, 1.0)
assert renormalized.tolist() == [2.0, 4.0, 6.0]
# array.array is buffer-protocol too, the non-owned path BorrowedF64 takes.
via_array = acme_core.normalize(array.array("d", [1.0, 2.0]), 3.0)
assert via_array.tolist() == [3.0, 6.0]

out = array.array("d", [0.0, 0.0, 0.0])
acme_core.scale_into([1.0, 2.0, 3.0], 2.0, out)
assert out.tolist() == [2.0, 4.0, 6.0]

assert acme_core.peak_level([0.1, 0.9, 0.3]) == Level.High
assert acme_core.peak_level([0.1, 0.2]) == Level.Low

found = acme_core.find_counter(5)
assert found.value == 5
assert acme_core.find_counter(-1) is None

assert acme_core.describe("world") == "label=world"
assert acme_core.describe(None) is None

assert acme_core.describe_owned("world") == "owned=world"
assert acme_core.describe_owned(None) is None

assert acme_core.maybe_ratio(4) == 0.25
assert acme_core.maybe_ratio(-1) is None

print("ok")
"#;

#[test]
#[ignore = "builds a real wheel with maturin; run with --ignored"]
fn the_python_golden_builds_and_runs() {
    if !support::require_toolchain(
        Command::new("maturin").arg("--version").output().is_ok(),
        "maturin",
        "pip install maturin, or uv tool install maturin",
    ) {
        return;
    }

    let scratch =
        std::env::temp_dir().join(format!("soothfast-python-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    // The glue's Cargo.toml names its dependency by `crate_path = ".."`, so
    // the acme crate has to sit at the golden's own parent directory.
    copy_dir(&manifest_dir().join("tests/fixture_crate"), &scratch);
    let glue = scratch.join("python");
    copy_dir(&golden_dir(), &glue);

    run(
        Command::new("cargo")
            .arg("generate-lockfile")
            .current_dir(&glue)
            .env_remove("CARGO_TARGET_DIR"),
        "cargo generate-lockfile",
    );
    run(
        Command::new("maturin")
            .args(["build", "--locked"])
            .current_dir(&glue)
            .env_remove("CARGO_TARGET_DIR"),
        "maturin build --locked",
    );

    let venv = glue.join("target/.smoke-venv");
    run(
        Command::new("python3").args(["-m", "venv"]).arg(&venv),
        "python3 -m venv",
    );
    let wheel_dir = glue.join("target/wheels");
    let wheel = std::fs::read_dir(&wheel_dir)
        .unwrap_or_else(|e| panic!("reads {}: {e}", wheel_dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "whl"))
        .expect("maturin build produced a wheel");
    run(
        Command::new(venv.join("bin/pip"))
            .arg("install")
            .arg(&wheel),
        "pip install",
    );

    let script = glue.join("smoke.py");
    std::fs::write(&script, SMOKE_SCRIPT).expect("writes smoke script");
    let output = Command::new(venv.join("bin/python"))
        .arg(&script)
        .output()
        .expect("runs python");
    assert!(
        output.status.success(),
        "python smoke script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("ok"));

    let _ = std::fs::remove_dir_all(&scratch);
}
