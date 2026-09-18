//! Builds the Kotlin golden's cdylib for real and runs Kotlin against it.
//!
//! Ignored by default: it shells out to cargo, kotlinc and kotlin, none of
//! which belong in the default `cargo test` loop. `kotlinc` is not on this
//! machine's PATH, so this test is expected to skip rather than run here.
//!
//! Run with: `cargo test -p soothfast-bind --test kotlin_smoke -- --ignored`

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).into()
}

/// Same scratch layout as `java_smoke.rs`: `fixture_crate` lands at the
/// scratch root and the golden lands one level under it, in `glue/`, since
/// the golden's `Cargo.toml` depends on `acme` at `..`.
fn build_scratch() -> PathBuf {
    let scratch = std::env::temp_dir().join(format!(
        "soothfast-bind-kotlin-smoke-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    copy_dir(&manifest_dir().join("tests/fixture_crate"), &scratch);
    copy_dir(
        &manifest_dir().join("tests/goldens/kotlin"),
        &scratch.join("glue"),
    );
    scratch
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("makes dir");
    for entry in std::fs::read_dir(from)
        .expect("reads dir")
        .filter_map(Result::ok)
    {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copies file");
        }
    }
}

fn smoke_source() -> PathBuf {
    manifest_dir().join("tests/kotlin_smoke/Smoke.kt")
}

/// Whether `kotlinc` is on `PATH`. Checked once up front so a missing
/// toolchain skips with one clear message instead of failing partway
/// through the build.
fn kotlinc_available() -> bool {
    match Command::new("kotlinc").arg("-version").output() {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("cannot run kotlinc: {e}"),
    }
}

#[test]
#[ignore = "shells out to cargo, kotlinc and kotlin"]
fn the_kotlin_golden_builds_and_runs() {
    if !support::require_toolchain(
        kotlinc_available(),
        "kotlinc",
        "https://kotlinlang.org/docs/command-line.html",
    ) {
        return;
    }

    let scratch = build_scratch();
    let glue = scratch.join("glue");

    let cdylib = build_cdylib(&glue);
    let lib_dir = glue.join("target/debug");
    let classes = compile_kotlin(&glue);

    let stdout = run_with_library_path(&classes, &lib_dir);
    assert_expected_output(&stdout, "");

    // Proves the jar-resource loading path, not just java.library.path: a
    // consumer who only has the jar on their classpath must still work.
    let jar_path = stage_jar(&glue, &classes, &cdylib);
    let stdout = run_via_jar(&jar_path);
    assert_expected_output(&stdout, " jar-only run");
}

fn build_cdylib(glue: &Path) -> PathBuf {
    let status = Command::new("cargo")
        .arg("build")
        .current_dir(glue)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("run cargo");
    assert!(status.success(), "cargo build failed in {}", glue.display());

    let lib_dir = glue.join("target/debug");
    let cdylib = ["libacme_core.so", "libacme_core.dylib", "acme_core.dll"]
        .iter()
        .map(|name| lib_dir.join(name))
        .find(|p| p.exists());
    assert!(cdylib.is_some(), "no cdylib built in {}", lib_dir.display());
    cdylib.unwrap()
}

fn compile_kotlin(glue: &Path) -> PathBuf {
    let classes = glue.join("target/smoke-classes");
    std::fs::create_dir_all(&classes).expect("makes dir");

    let mut kotlinc = Command::new("kotlinc");
    kotlinc.arg("-d").arg(&classes);
    kotlinc.args(kotlin_sources(&glue.join("src/main/kotlin")));
    kotlinc.arg(smoke_source());
    let status = kotlinc.status().expect("run kotlinc");
    assert!(status.success(), "kotlinc failed");
    classes
}

fn run_with_library_path(classes: &Path, lib_dir: &Path) -> String {
    let output = Command::new("kotlin")
        .arg("-J--enable-native-access=ALL-UNNAMED")
        .arg(format!("-J-Djava.library.path={}", lib_dir.display()))
        .arg("-cp")
        .arg(classes)
        .arg("SmokeKt")
        .output()
        .expect("run kotlin");
    assert!(
        output.status.success(),
        "kotlin exited {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stage_jar(glue: &Path, classes: &Path, cdylib: &Path) -> PathBuf {
    let jar_staging = glue.join("target/smoke-jar-staging");
    let native_dir = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let natives_dir = jar_staging.join("natives").join(&native_dir);
    std::fs::create_dir_all(&natives_dir).expect("makes dir");
    let cdylib_name = cdylib.file_name().expect("cdylib has a name");
    std::fs::copy(cdylib, natives_dir.join(cdylib_name)).expect("copies cdylib");

    let jar_path = glue.join("target/smoke.jar");
    let mut jar_cmd = Command::new("jar");
    jar_cmd.arg("--create").arg("--file").arg(&jar_path);
    jar_cmd.arg("-C").arg(classes).arg(".");
    jar_cmd.arg("-C").arg(&jar_staging).arg("natives");
    let status = jar_cmd.status().expect("run jar");
    assert!(status.success(), "jar failed");
    jar_path
}

fn run_via_jar(jar_path: &Path) -> String {
    let output = Command::new("kotlin")
        .arg("-J--enable-native-access=ALL-UNNAMED")
        .arg("-cp")
        .arg(jar_path)
        .arg("SmokeKt")
        .output()
        .expect("run kotlin");
    assert!(
        output.status.success(),
        "kotlin (jar-only) exited {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn assert_expected_output(stdout: &str, label: &str) {
    for expected in EXPECTED_OUTPUT {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in{label}: {stdout}"
        );
    }
}

const EXPECTED_OUTPUT: &[&str] = &[
    "value=10",
    "bump=15",
    "at(Low)=10",
    "at(High)=20",
    "bumpAll=16",
    "caught: overflow",
    "digest=[2, 3, 4]",
    "normalize=[2.0, 4.0, 6.0]",
    "scaleInto=[2.0, 4.0, 6.0]",
    "stamp=12",
    "peakLevel=High",
    "findCounter=5",
    "findCounter(missing)=null",
    "describe=label=world",
    "describe(none)=null",
    "describeOwned=owned=world",
    "describeOwned(none)=null",
    "split.lo=[2.0, 4.0, 6.0]",
    "split.hi=[-1.0, -2.0, -3.0]",
];

fn kotlin_sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)
        .expect("reads dir")
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.is_dir() {
            out.extend(kotlin_sources(&path));
        } else if path.extension().is_some_and(|e| e == "kt") {
            out.push(path);
        }
    }
    out
}
