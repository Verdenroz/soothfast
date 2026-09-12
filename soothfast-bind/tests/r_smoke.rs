//! Installs the R golden for real and runs R against it.
//!
//! Ignored by default: it shells out to `R CMD INSTALL` and `Rscript`,
//! neither of which belong in the default `cargo test` loop.
//!
//! Run with: `cargo test -p soothfast-bind --test r_smoke -- --ignored`

use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).into()
}

/// The golden's `src/rust/Cargo.toml` depends on `acme` at `../../..`, the
/// layout `bind gen` actually produces (the glue crate sits inside the bound
/// crate's own tree, three directories under `src/rust/`). Scratch mirrors
/// that: `fixture_crate` lands at the scratch root and the golden lands one
/// level under it, in `glue/`, the same as every other smoke test.
fn build_scratch() -> PathBuf {
    let scratch =
        std::env::temp_dir().join(format!("soothfast-bind-r-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    copy_dir(&manifest_dir().join("tests/fixture_crate"), &scratch);
    copy_dir(
        &manifest_dir().join("tests/goldens/r"),
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
    manifest_dir().join("tests/r_smoke/smoke.R")
}

#[test]
#[ignore = "shells out to R CMD INSTALL and Rscript"]
fn the_r_golden_installs_and_runs() {
    let scratch = build_scratch();
    let glue = scratch.join("glue");
    let library = scratch.join("rlib");
    std::fs::create_dir_all(&library).expect("makes dir");

    let status = Command::new("R")
        .arg("CMD")
        .arg("INSTALL")
        .arg("--no-multiarch")
        .arg(format!("--library={}", library.display()))
        .arg(&glue)
        .status()
        .expect("run R CMD INSTALL");
    assert!(
        status.success(),
        "R CMD INSTALL failed for {}",
        glue.display()
    );

    let output = Command::new("Rscript")
        .arg("--vanilla")
        .arg(smoke_source())
        .arg(&library)
        .output()
        .expect("run Rscript");
    assert!(
        output.status.success(),
        "Rscript exited {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
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
    "bump_all=16",
    "digest=02 03 04",
    "normalize=2 4 6",
    "greet=hello, R",
    "stamp=6",
    "trim(empty)=TRUE",
    "trim=1 2",
    "caught: unknown Level variant",
];
