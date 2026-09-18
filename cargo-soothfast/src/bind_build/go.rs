use std::path::Path;
use std::process::Command;

use super::cargo_build::cargo;
use super::staging::hush;

/// The C backend's own build, then `go vet`/`go build` over the wrapper
/// generated against it. A missing `go` toolchain skips the verification
/// rather than failing the cdylib the cargo build already produced.
pub(super) fn go(
    glue: &Path,
    targets: &[String],
    release: bool,
    quiet: bool,
) -> Result<Vec<String>, String> {
    let out = cargo(glue, targets, release, quiet)?;
    for args in [["vet", "./..."], ["build", "./..."]] {
        let mut cmd = Command::new("go");
        cmd.args(args).current_dir(glue).env("CGO_ENABLED", "1");
        hush(&mut cmd, quiet);
        let status = cmd.status();
        match status {
            Ok(status) if status.success() => {}
            Ok(_) => return Err(format!("`go {}` failed", args.join(" "))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "soothfast: `go` not found — https://go.dev/doc/install; \
                     skipping wrapper verification"
                );
                break;
            }
            Err(e) => return Err(format!("cannot run go: {e}")),
        }
    }
    Ok(out)
}
