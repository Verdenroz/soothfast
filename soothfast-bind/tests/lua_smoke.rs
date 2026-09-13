//! End-to-end: build the Lua golden's cdylib for real, then run `Smoke.lua`
//! against it through the actual LuaJIT `ffi` path.
//!
//! Ignored by default since it shells out to `cargo build --release` and
//! `luajit`. Run with:
//! `cargo test -p soothfast-bind --test lua_smoke -- --ignored`
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

#[test]
#[ignore = "shells out to cargo and luajit; run with --ignored"]
fn the_lua_golden_builds_and_runs_against_the_real_cdylib() {
    let manifest = manifest_dir();
    let root = std::env::temp_dir().join(format!("soothfast-lua-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    copy_dir(&manifest.join("tests/fixture_crate"), &root);

    let glue = root.join("glue");
    copy_dir(&manifest.join("tests/goldens/lua"), &glue);
    std::fs::copy(manifest.join("tests/lua/Smoke.lua"), glue.join("Smoke.lua"))
        .expect("copies the smoke script");

    // The library the smoke script loads lands in the glue crate's own
    // `target/release`, never wherever the outer test run points
    // CARGO_TARGET_DIR.
    let build = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&glue)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("runs cargo build");
    assert!(build.success(), "cargo build --release failed");

    let luajit = Command::new("luajit")
        .arg("Smoke.lua")
        .current_dir(&glue)
        .env("LUA_PATH", "./?.lua;;")
        .env("LD_LIBRARY_PATH", glue.join("target/release"))
        .output()
        .expect("runs luajit");
    assert!(
        luajit.status.success(),
        "luajit exited {:?}: {}",
        luajit.status.code(),
        String::from_utf8_lossy(&luajit.stderr)
    );
    assert!(String::from_utf8_lossy(&luajit.stdout).contains("ok"));

    let _ = std::fs::remove_dir_all(&root);
}
