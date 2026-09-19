//! `bind gate` and `bind gen --check` driven to an actual failure against a
//! real, minimal `#[soothfast::export]` crate in a throwaway git repo:
//! everywhere else exercises `compat::diff` and the lockfile check in
//! isolation, never the CLI commands that wrap them.
//!
//! Ignored by default: both need the pinned nightly rustdoc toolchain
//! (`SOOTHFAST_RUSTDOC_TOOLCHAIN`), the same one `soothfast-bind`'s
//! `fixture_parity` test needs. Run with:
//! `cargo test -p cargo-soothfast --test bind_gate_smoke -- --ignored`

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn soothfast_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../soothfast")
        .canonicalize()
        .expect("the soothfast crate is a workspace sibling")
}

/// `soothfast-demo`'s own `Cargo.toml`/`benches/soothfast.rs` shape, with
/// one exported free fn whose arity the caller controls.
fn write_fixture(dir: &Path, params: &str, body: &str) {
    fs::create_dir_all(dir.join("src")).expect("makes src dir");
    fs::create_dir_all(dir.join("benches")).expect("makes benches dir");
    let soothfast = soothfast_path();
    let soothfast = soothfast.display();
    fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"fake-pkg\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             publish = false\n\n\
             [dependencies]\nsoothfast = {{ path = \"{soothfast}\" }}\n\n\
             [dev-dependencies]\nsoothfast = {{ path = \"{soothfast}\", features = [\"runner\"] }}\n\n\
             [[bench]]\nname = \"soothfast\"\nharness = false\n"
        ),
    )
    .expect("writes Cargo.toml");
    fs::write(
        dir.join("src/lib.rs"),
        format!("#[soothfast::export]\npub fn add({params}) -> i64 {{\n    {body}\n}}\n"),
    )
    .expect("writes src/lib.rs");
    fs::write(
        dir.join("benches/soothfast.rs"),
        "use fake_pkg::add as _;\n\nsoothfast::bench_main!();\n",
    )
    .expect("writes benches/soothfast.rs");
    fs::write(
        dir.join("soothfast.toml"),
        "[[bind]]\nlang = \"c\"\nout = \"bindings/c\"\npackage = \"fake-pkg-c\"\n",
    )
    .expect("writes soothfast.toml");
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.email=test@example.com", "-c", "user.name=test"])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("runs git");
    assert!(status.success(), "git {args:?} failed");
}

fn commit_all(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("runs git rev-parse");
    String::from_utf8(out.stdout)
        .expect("git rev-parse prints utf8")
        .trim()
        .to_string()
}

fn scratch(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("soothfast-bind-{tag}-smoke-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// A `bind` subcommand against `dir`. `CARGO_TARGET_DIR` from the outer
/// `cargo test` run would otherwise point the child build (and its rustdoc
/// JSON cache) at a directory this fixture crate never touches, so its
/// freshness check would never see the fixture's own edits.
fn bind_cmd(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cargo-soothfast"));
    cmd.arg("bind").args(args);
    cmd.current_dir(dir).env_remove("CARGO_TARGET_DIR");
    cmd
}

#[test]
#[ignore = "needs the pinned nightly rustdoc toolchain; shells out to cargo and git"]
fn bind_gate_fails_on_a_breaking_change_and_passes_with_allow_breaking() {
    let dir = scratch("gate");
    write_fixture(&dir, "a: i64, b: i64", "a + b");
    git(&dir, &["init", "-q"]);
    let base = commit_all(&dir, "base");

    write_fixture(&dir, "a: i64, b: i64, c: i64", "a + b + c");
    commit_all(&dir, "breaking change: add a third parameter");

    let out = bind_cmd(&dir, &["gate", "-p", "fake-pkg", "--base", &base])
        .output()
        .expect("runs bind gate");
    assert!(
        !out.status.success(),
        "bind gate should fail on a breaking change"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("BREAK"), "missing BREAK in: {stdout}");

    let status = bind_cmd(
        &dir,
        &[
            "gate",
            "-p",
            "fake-pkg",
            "--base",
            &base,
            "--allow-breaking",
        ],
    )
    .status()
    .expect("runs bind gate --allow-breaking");
    assert!(status.success(), "bind gate --allow-breaking should pass");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "needs the pinned nightly rustdoc toolchain; shells out to cargo and git"]
fn bind_gen_check_fails_on_a_stale_committed_file() {
    let dir = scratch("check");
    write_fixture(&dir, "a: i64, b: i64", "a + b");
    git(&dir, &["init", "-q"]);
    commit_all(&dir, "base");

    let status = bind_cmd(&dir, &["gen", "-p", "fake-pkg"])
        .status()
        .expect("runs bind gen");
    assert!(status.success(), "bind gen failed");
    commit_all(&dir, "generated bindings");

    let header = dir.join("bindings/c/fake_pkg_c.h");
    let original = fs::read_to_string(&header).expect("reads the generated header");
    let mutated = original.replacen("int64_t a, int64_t b)", "int64_t a, int64_t bx)", 1);
    assert_ne!(
        original, mutated,
        "header no longer has the signature to mutate"
    );
    fs::write(&header, mutated).expect("writes the mutated header");

    let out = bind_cmd(&dir, &["gen", "-p", "fake-pkg", "--check"])
        .output()
        .expect("runs bind gen --check");
    assert!(
        !out.status.success(),
        "bind gen --check should fail on a stale file"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("fake_pkg_c.h"),
        "missing the stale file's name in: {stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}
