//! End-to-end: build the C++ golden's cdylib for real, then compile and run
//! `Smoke.cpp` against its header and library through a real linker.
//!
//! Ignored by default since it shells out to `cargo build --release` and a
//! C++ compiler. Run with:
//! `cargo test -p soothfast-bind --test cpp_smoke -- --ignored`
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

/// `$CXX` first, then the usual PATH names, the same order `bind build`
/// looks for one.
fn cpp_compiler() -> Option<String> {
    std::env::var("CXX").ok().or_else(|| {
        ["c++", "g++", "clang++"]
            .into_iter()
            .find(|c| Command::new(c).arg("--version").output().is_ok())
            .map(str::to_string)
    })
}

#[test]
#[ignore]
fn the_cpp_golden_builds_and_runs_against_the_real_cdylib() {
    let Some(compiler) = cpp_compiler() else {
        eprintln!("no c++ compiler on PATH; skipping");
        return;
    };

    let manifest = manifest_dir();
    let root = std::env::temp_dir().join(format!("soothfast-cpp-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    copy_dir(&manifest.join("tests/fixture_crate"), &root);

    let glue = root.join("glue");
    copy_dir(&manifest.join("tests/goldens/cpp"), &glue);

    let build = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&glue)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("runs cargo build");
    assert!(build.success(), "cargo build --release failed");

    let lib_dir = glue.join("target/release");
    let binary = glue.join("smoke");
    let compile = Command::new(&compiler)
        .arg("-std=c++20")
        .arg(manifest.join("tests/cpp/Smoke.cpp"))
        .arg("-I")
        .arg(&glue)
        .arg("-L")
        .arg(&lib_dir)
        .arg("-lcore")
        .arg(format!("-Wl,-rpath,{}", lib_dir.display()))
        .arg("-o")
        .arg(&binary)
        .status()
        .expect("runs the c++ compiler");
    assert!(compile.success(), "{compiler} failed to compile Smoke.cpp");

    let output = Command::new(&binary)
        .output()
        .expect("runs the smoke binary");
    assert!(
        output.status.success(),
        "smoke binary exited {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in EXPECTED_OUTPUT {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in: {stdout}"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}

const EXPECTED_OUTPUT: &[&str] = &[
    "value=10",
    "at(Low)=10",
    "at(High)=20",
    "bump=15",
    "bumpAll=16",
    "caught: overflow",
    "digest=[2, 3, 4]",
    "normalize=[2, 4, 6]",
    "greet=hello, C++",
    "stamp=6",
    "peak_level=High",
    "find_counter=5",
    "find_counter(missing)=none",
    "describe=label=world",
    "describe(none)=none",
];
