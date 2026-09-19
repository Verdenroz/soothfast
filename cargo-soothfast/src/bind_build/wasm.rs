use std::path::Path;
use std::process::Command;

use super::staging::{artifacts, hush};

/// One `wasm-pack build`, whatever the configured targets say.
///
/// A `.wasm` has no os/cpu/libc axis, so the triples a Python package needs
/// mean nothing here and naming any is a mistake worth reporting.
pub(super) fn wasm_pack(
    glue: &Path,
    targets: &[String],
    release: bool,
    quiet: bool,
) -> Result<Vec<String>, String> {
    if !targets.is_empty() {
        eprintln!(
            "soothfast: ignoring --target for wasm; one .wasm runs on every \
             platform, so there is no matrix to build"
        );
    }
    let mut args = vec!["build", "--target", "web"];
    if release {
        args.push("--release");
    }
    // Everything after `--` is passed straight through to `cargo build`.
    args.extend(["--", "--locked"]);
    let mut cmd = Command::new("wasm-pack");
    cmd.args(&args)
        .current_dir(glue)
        // wasm-pack shells out to cargo itself; same CARGO_TARGET_DIR
        // reasoning as the C backend and maturin.
        .env_remove("CARGO_TARGET_DIR");
    // `[build] rustflags` still reaches the wasm32 link and an empty target
    // override reads as unset, so a no-op flag stands in.
    if std::env::var_os("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS").is_none() {
        cmd.env(
            "CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS",
            "-Cstrip=none",
        );
    }
    hush(&mut cmd, quiet);
    let status = cmd.status().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            "`wasm-pack` not found: cargo install wasm-pack".to_string()
        }
        _ => format!("cannot run wasm-pack: {e}"),
    })?;
    if !status.success() {
        return Err(format!("`wasm-pack {}` failed", args.join(" ")));
    }
    artifacts(&glue.join("pkg"), &["wasm", "js", "ts"])
}
