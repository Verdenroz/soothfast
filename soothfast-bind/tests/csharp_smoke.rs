//! Builds the C# golden's cdylib for real and runs a .NET console app
//! against it.
//!
//! Ignored by default: it shells out to cargo and dotnet, neither of which
//! belong in the default `cargo test` loop. `dotnet` is not on this
//! machine's PATH, so this test is expected to skip rather than run here.
//!
//! Run with: `cargo test -p soothfast-bind --test csharp_smoke -- --ignored`

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).into()
}

/// Same scratch layout as `kotlin_smoke.rs`: `fixture_crate` lands at the
/// scratch root and the golden lands one level under it, in `glue/`, since
/// the golden's `Cargo.toml` depends on `acme` at `..`.
fn build_scratch() -> PathBuf {
    let scratch = std::env::temp_dir().join(format!(
        "soothfast-bind-csharp-smoke-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    copy_dir(&manifest_dir().join("tests/fixture_crate"), &scratch);
    copy_dir(
        &manifest_dir().join("tests/goldens/csharp"),
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

fn smoke_dir() -> PathBuf {
    manifest_dir().join("tests/csharp_smoke")
}

/// Whether `dotnet` is on `PATH`. Checked once up front so a missing
/// toolchain skips with one clear message instead of failing partway
/// through the build.
fn dotnet_available() -> bool {
    match Command::new("dotnet").arg("--version").output() {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("cannot run dotnet: {e}"),
    }
}

#[test]
#[ignore = "shells out to cargo and dotnet"]
fn the_csharp_golden_builds_and_runs() {
    if !support::require_toolchain(
        dotnet_available(),
        "dotnet",
        "https://dotnet.microsoft.com/download",
    ) {
        return;
    }

    let scratch = build_scratch();
    let glue = scratch.join("glue");

    let cdylib = build_cdylib(&glue);
    let smoke_dll = build_dotnet_smoke(&glue, &cdylib);

    let stdout = run_dotnet(&smoke_dll);
    assert_expected_output(&stdout);
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

fn build_dotnet_smoke(glue: &Path, cdylib: &Path) -> PathBuf {
    let smoke = glue.join("smoke");
    copy_dir(&smoke_dir(), &smoke);

    let status = Command::new("dotnet")
        .arg("build")
        .current_dir(&smoke)
        .status()
        .expect("run dotnet build");
    assert!(status.success(), "dotnet build failed");

    let output_dir = smoke.join("bin/Debug/net8.0");
    let cdylib_name = cdylib.file_name().expect("cdylib has a name");
    std::fs::copy(cdylib, output_dir.join(cdylib_name)).expect("copies cdylib");
    output_dir.join("Smoke.dll")
}

fn run_dotnet(smoke_dll: &Path) -> String {
    let output = Command::new("dotnet")
        .arg(smoke_dll)
        .output()
        .expect("run dotnet");
    assert!(
        output.status.success(),
        "dotnet exited {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn assert_expected_output(stdout: &str) {
    for expected in EXPECTED_OUTPUT {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in: {stdout}"
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
    "normalize=[2, 4, 6]",
    "scaleInto=[2, 4, 6]",
    "stamp=12",
    "peakLevel=High",
    "findCounter=5",
    "findCounter(missing)=null",
    "describe=label=world",
    "describe(none)=null",
    "describeOwned=owned=world",
    "describeOwned(none)=null",
    "fail caught: bad byte\u{fffd}end",
    "split.lo=[2, 4, 6]",
    "split.hi=[-1, -2, -3]",
    "use after close: Counter",
];
