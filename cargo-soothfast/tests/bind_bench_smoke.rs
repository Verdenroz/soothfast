//! End-to-end `bind bench` smoke test: a fake `[[bind]]` entry whose
//! `bench` script is a shell script printing canned readings, so it runs
//! with no host toolchain beyond a POSIX shell.

use std::fs;
use std::path::Path;
use std::process::Command;

/// `bind bench` builds its target before launching a script (the same
/// `bind build` a real `[[bind]]` entry goes through), so the `c` glue
/// directory needs a real, if trivial, cdylib crate — not just a script.
/// `bind build` passes `--locked`, so a fixture glue crate needs the lockfile
/// `bind gen` would have written beside its manifest.
fn lock(glue: &Path) {
    let status = Command::new("cargo")
        .arg("generate-lockfile")
        .current_dir(glue)
        .status()
        .expect("runs cargo generate-lockfile");
    assert!(status.success(), "cargo generate-lockfile failed");
}

fn write_fixture(dir: &Path) {
    fs::create_dir_all(dir.join("src")).expect("makes src dir");
    fs::create_dir_all(dir.join("bindings/c/src")).expect("makes glue dir");
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"fake-pkg\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("writes Cargo.toml");
    fs::write(dir.join("src/lib.rs"), "").expect("writes src/lib.rs");
    fs::write(
        dir.join("soothfast.toml"),
        "[[bind]]\nlang = \"c\"\nout = \"bindings/c\"\npackage = \"fake-pkg\"\n\
         bench = \"bench.sh\"\n",
    )
    .expect("writes soothfast.toml");
    fs::write(
        dir.join("bindings/c/Cargo.toml"),
        "[package]\nname = \"fake-glue\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [lib]\nname = \"fake_glue\"\ncrate-type = [\"cdylib\"]\n",
    )
    .expect("writes glue Cargo.toml");
    fs::write(dir.join("bindings/c/src/lib.rs"), "").expect("writes glue src/lib.rs");
    lock(&dir.join("bindings/c"));

    let script = dir.join("bench.sh");
    fs::write(
        &script,
        "#!/bin/sh\n\
         echo '{\"shape\": \"build_summary\", \"binding_ns\": 10.0, \"host_ns\": 40.0, \"n\": 100000}'\n\
         echo '{\"shape\": \"batch_buffer\", \"binding_ns\": 5.0, \"host_ns\": 45.0, \"n\": 100000}'\n",
    )
    .expect("writes bench.sh");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&script)
        .expect("reads permissions")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("sets permissions");
}

#[test]
fn measures_a_fake_entry_and_saves_a_baseline() {
    let dir =
        std::env::temp_dir().join(format!("soothfast-bind-bench-smoke-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    write_fixture(&dir);

    let out = Command::new(env!("CARGO_BIN_EXE_cargo-soothfast"))
        .args([
            "bind",
            "bench",
            "-p",
            "fake-pkg",
            "--save-baseline",
            "smoke",
        ])
        .current_dir(&dir)
        .output()
        .expect("runs cargo-soothfast");
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("build_summary"), "{stdout}");
    assert!(stdout.contains("batch_buffer"), "{stdout}");
    assert!(stdout.contains("2 shape(s) measured"), "{stdout}");

    let baseline_path = dir.join(".soothfast/baselines/smoke.json");
    let baseline: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&baseline_path).expect("baseline written"))
            .expect("valid json");
    let item = &baseline["items"]["fake_pkg::bind::c::build_summary"];
    assert_eq!(item["ratio"]["ratio"].as_f64(), Some(4.0));
    assert_eq!(item["ratio"]["binding_ns"].as_f64(), Some(10.0));
    assert_eq!(item["ratio"]["host_ns"].as_f64(), Some(40.0));
    assert_eq!(item["ratio"]["n"].as_u64(), Some(100000));

    let _ = fs::remove_dir_all(&dir);
}

/// An inherited `CARGO_TARGET_DIR` must not move the glue crate's build
/// output away from where `bind bench` looks for it (the same reason
/// `bind_build`'s cargo, maturin, napi and wasm-pack calls all strip it).
#[test]
fn builds_correctly_even_with_cargo_target_dir_set_in_the_environment() {
    let dir = std::env::temp_dir().join(format!(
        "soothfast-bind-bench-target-dir-smoke-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    write_fixture(&dir);

    let out = Command::new(env!("CARGO_BIN_EXE_cargo-soothfast"))
        .args(["bind", "bench", "-p", "fake-pkg"])
        .current_dir(&dir)
        .env("CARGO_TARGET_DIR", dir.join("outer-target"))
        .output()
        .expect("runs cargo-soothfast");
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("2 shape(s) measured"), "{stdout}");

    let _ = fs::remove_dir_all(&dir);
}

/// Adds a second `[[bind]]` entry, `bindings/c2`, whose script always exits
/// non-zero, to a directory `write_fixture` already populated.
fn add_failing_entry(dir: &Path) {
    fs::create_dir_all(dir.join("bindings/c2/src")).expect("makes second glue dir");
    let mut soothfast_toml = fs::read_to_string(dir.join("soothfast.toml")).expect("reads config");
    soothfast_toml.push_str(
        "\n[[bind]]\nlang = \"c\"\nout = \"bindings/c2\"\npackage = \"fake-pkg\"\n\
         bench = \"bench_fails.sh\"\n",
    );
    fs::write(dir.join("soothfast.toml"), soothfast_toml).expect("appends config");
    fs::write(
        dir.join("bindings/c2/Cargo.toml"),
        "[package]\nname = \"fake-glue-2\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [lib]\nname = \"fake_glue_2\"\ncrate-type = [\"cdylib\"]\n",
    )
    .expect("writes second glue Cargo.toml");
    fs::write(dir.join("bindings/c2/src/lib.rs"), "").expect("writes second glue src/lib.rs");
    lock(&dir.join("bindings/c2"));

    let script = dir.join("bench_fails.sh");
    fs::write(&script, "#!/bin/sh\nexit 1\n").expect("writes bench_fails.sh");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&script)
        .expect("reads permissions")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("sets permissions");
}

#[test]
fn one_entrys_failure_does_not_hide_another_entrys_results() {
    let dir = std::env::temp_dir().join(format!(
        "soothfast-bind-bench-partial-failure-smoke-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    write_fixture(&dir);
    add_failing_entry(&dir);

    let out = Command::new(env!("CARGO_BIN_EXE_cargo-soothfast"))
        .args(["bind", "bench", "-p", "fake-pkg"])
        .current_dir(&dir)
        .output()
        .expect("runs cargo-soothfast");
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("build_summary"), "{stdout}");
    assert!(stdout.contains("2 shape(s) measured"), "{stdout}");
    assert!(stdout.contains("FAILED"), "{stdout}");
    assert!(stdout.contains("bindings/c2"), "{stdout}");

    let _ = fs::remove_dir_all(&dir);
}

/// A `[[bind]]` entry of `lang`, with a cdylib glue crate at `out` (so `bind
/// build` has something to compile) and `bench_body` written to `bench_file`
/// as its bench script.
fn write_entry_fixture(dir: &Path, lang: &str, out: &str, bench_file: &str, bench_body: &str) {
    fs::create_dir_all(dir.join(out).join("src")).expect("makes glue dir");
    fs::write(
        dir.join(out).join("Cargo.toml"),
        "[package]\nname = \"fake-glue-2\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [lib]\nname = \"fake_glue_2\"\ncrate-type = [\"cdylib\"]\n",
    )
    .expect("writes glue Cargo.toml");
    fs::write(dir.join(out).join("src/lib.rs"), "").expect("writes glue src/lib.rs");
    lock(&dir.join(out));
    fs::write(dir.join(bench_file), bench_body).expect("writes bench script");
    let mut soothfast_toml = fs::read_to_string(dir.join("soothfast.toml")).expect("reads config");
    soothfast_toml.push_str(&format!(
        "\n[[bind]]\nlang = \"{lang}\"\nout = \"{out}\"\npackage = \"fake-pkg\"\nbench = \"{bench_file}\"\n"
    ));
    fs::write(dir.join("soothfast.toml"), soothfast_toml).expect("appends config");
}

const CANNED_JSON_CPP: &str = r#"#include <cstdio>
int main() {
    std::puts("{\"shape\": \"build_summary\", \"binding_ns\": 10.0, \"host_ns\": 40.0, \"n\": 100000}");
    std::puts("{\"shape\": \"batch_buffer\", \"binding_ns\": 5.0, \"host_ns\": 45.0, \"n\": 100000}");
    return 0;
}
"#;

/// `bind bench` compiles the C++ bench source itself (there is no separate
/// `bind build` step for the host language); a machine with no C++ compiler
/// skips the entry rather than failing the run.
#[test]
fn measures_a_cpp_entry_or_skips_without_a_compiler() {
    let dir = std::env::temp_dir().join(format!(
        "soothfast-bind-bench-cpp-smoke-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    write_fixture(&dir);
    write_entry_fixture(
        &dir,
        "cpp",
        "bindings/cpp",
        "bindings/cpp/bench.cpp",
        CANNED_JSON_CPP,
    );

    let out = Command::new(env!("CARGO_BIN_EXE_cargo-soothfast"))
        .args(["bind", "bench", "-p", "fake-pkg", "--only", "cpp"])
        .current_dir(&dir)
        .output()
        .expect("runs cargo-soothfast");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("build_summary") || stdout.contains("skipping"),
        "{stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}

const CANNED_JSON_LUA: &str = r#"print('{"shape": "build_summary", "binding_ns": 10.0, "host_ns": 40.0, "n": 100000}')
print('{"shape": "batch_buffer", "binding_ns": 5.0, "host_ns": 45.0, "n": 100000}')
"#;

/// A machine with no `luajit` skips the entry rather than failing the run.
#[test]
fn measures_a_lua_entry_or_skips_without_luajit() {
    let dir = std::env::temp_dir().join(format!(
        "soothfast-bind-bench-lua-smoke-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    write_fixture(&dir);
    write_entry_fixture(
        &dir,
        "lua",
        "bindings/lua",
        "bindings/lua/bench.lua",
        CANNED_JSON_LUA,
    );

    let out = Command::new(env!("CARGO_BIN_EXE_cargo-soothfast"))
        .args(["bind", "bench", "-p", "fake-pkg", "--only", "lua"])
        .current_dir(&dir)
        .output()
        .expect("runs cargo-soothfast");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("build_summary") || stdout.contains("skipping"),
        "{stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn only_filters_out_a_non_matching_entry() {
    let dir = std::env::temp_dir().join(format!(
        "soothfast-bind-bench-only-smoke-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    write_fixture(&dir);

    let out = Command::new(env!("CARGO_BIN_EXE_cargo-soothfast"))
        .args(["bind", "bench", "-p", "fake-pkg", "--only", "python"])
        .current_dir(&dir)
        .output()
        .expect("runs cargo-soothfast");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("nothing to bench"),
        "expected the c entry to be filtered out: {stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}
