//! `cargo soothfast sdk build` — cross-compile the embedded server and
//! stage it into each SDK's platform artifacts.
//!
//! One binary per target, then whatever that language's packaging needs:
//! npm wants a directory per platform package, Python wants one wheel per
//! platform tag. Targets whose toolchain is missing are reported and
//! skipped rather than failing the run — a laptop rarely has all five.

use std::path::{Path, PathBuf};
use std::process::Command;

use soothfast_sdk::SdkKind;
use soothfast_sdk::target::Target;

/// What a build produced for one target.
pub struct Built {
    pub target: &'static Target,
    pub binary: PathBuf,
}

/// Cross-compile `binary` for each target, returning what succeeded
/// alongside a human-readable reason for each that did not.
pub fn compile(
    pkg: &str,
    binary: &str,
    targets: &[&'static Target],
    release: bool,
    target_dir: &Path,
) -> (Vec<Built>, Vec<String>) {
    let mut built = Vec::new();
    let mut skipped = Vec::new();
    for target in targets {
        match compile_one(pkg, binary, target, release, target_dir) {
            Ok(path) => built.push(Built {
                target,
                binary: path,
            }),
            Err(why) => skipped.push(format!("{}: {why}", target.triple)),
        }
    }
    (built, skipped)
}

fn compile_one(
    pkg: &str,
    binary: &str,
    target: &Target,
    release: bool,
    target_dir: &Path,
) -> Result<PathBuf, String> {
    let mut cmd = Command::new("cargo");
    // `-p` is not optional: without it cargo resolves `--bin` against the
    // workspace's default-run packages, and a server that lives in a
    // non-default member is simply not found.
    cmd.args([
        "build",
        "-p",
        pkg,
        "--bin",
        binary,
        "--target",
        target.triple,
    ]);
    if release {
        cmd.arg("--release");
    }
    let status = cmd.status().map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "cargo build failed — is the target installed? \
             (rustup target add {})",
            target.triple
        ));
    }
    let path = binary_path(target_dir, binary, target, release);
    if !path.is_file() {
        return Err(format!("built, but {} is missing", path.display()));
    }
    Ok(path)
}

/// Where `cargo build --target T` leaves a binary. Cross-compiled output
/// nests under the triple, which host-only builds do not.
fn binary_path(target_dir: &Path, binary: &str, target: &Target, release: bool) -> PathBuf {
    target_dir
        .join(target.triple)
        .join(if release { "release" } else { "debug" })
        .join(format!("{binary}{}", target.exe_suffix))
}

/// Copy each binary into its npm platform package, next to the manifest
/// `sdk gen` already emitted there.
pub fn stage_npm(
    out_dir: &Path,
    package: &str,
    binary: &str,
    built: &[Built],
) -> Result<Vec<String>, String> {
    let mut staged = Vec::new();
    for item in built {
        let dir = out_dir
            .join("platforms")
            .join(format!("{package}-{}", item.target.npm_suffix()))
            .join("bin");
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let dest = dir.join(format!("{binary}{}", item.target.exe_suffix));
        std::fs::copy(&item.binary, &dest)
            .map_err(|e| format!("cannot stage {}: {e}", dest.display()))?;
        make_executable(&dest)?;
        staged.push(item.target.triple.to_string());
    }
    Ok(staged)
}

/// Build one platform wheel per target, plus the sdist, into `<out>/dist`.
pub fn build_wheels(out_dir: &Path, built: &[Built]) -> Result<Vec<String>, String> {
    let dist = out_dir.join("dist");
    // Stale wheels from a previous matrix would be published alongside the
    // new ones, so start from an empty directory.
    let _ = std::fs::remove_dir_all(&dist);
    std::fs::create_dir_all(&dist).map_err(|e| format!("cannot create {}: {e}", dist.display()))?;

    run_uv(&["build", "--sdist", "--out-dir", "dist"], out_dir, &[])?;
    let mut wheels = Vec::new();
    for item in built {
        let absolute = std::fs::canonicalize(&item.binary)
            .map_err(|e| format!("cannot resolve {}: {e}", item.binary.display()))?;
        run_uv(
            &["build", "--wheel", "--out-dir", "dist"],
            out_dir,
            &[
                ("SOOTHFAST_SERVER_BIN", absolute.to_string_lossy().as_ref()),
                ("SOOTHFAST_WHEEL_TAG", &item.target.wheel_tag()),
            ],
        )?;
        wheels.push(item.target.wheel_tag());
    }
    Ok(wheels)
}

/// A `manylinux_*` tag asserts the binary links against a glibc at least
/// that old. Only a manylinux build container can honor that, and it
/// advertises itself through `AUDITWHEEL_PLAT`.
pub fn manylinux_warning(targets: &[&'static Target]) -> Option<String> {
    manylinux_warning_for(targets, std::env::var_os("AUDITWHEEL_PLAT").is_some())
}

fn manylinux_warning_for(targets: &[&'static Target], in_manylinux: bool) -> Option<String> {
    if in_manylinux {
        return None;
    }
    let claimed: Vec<&str> = targets
        .iter()
        .map(|t| t.wheel_platform)
        .filter(|p| p.starts_with("manylinux"))
        .collect();
    if claimed.is_empty() {
        return None;
    }
    Some(format!(
        "tagging {} outside a manylinux container — the wheel claims an \
         older glibc than it was linked against, and will fail to start on \
         systems that honor the tag. Build these in the manylinux image \
         (or switch those targets to musl) before publishing.",
        claimed.join(", ")
    ))
}

fn run_uv(args: &[&str], dir: &Path, env: &[(&str, &str)]) -> Result<(), String> {
    let mut cmd = Command::new("uv");
    cmd.args(args).current_dir(dir);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let status = cmd.status().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "`uv` not found — curl -LsSf https://astral.sh/uv/install.sh | sh".to_string()
        } else {
            format!("cannot run uv: {e}")
        }
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`uv {}` failed", args.join(" ")))
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    // `fs::copy` carries the mode over, but a binary staged from a
    // non-Unix source or an odd umask would arrive unrunnable.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("cannot chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Whether an SDK has binaries staged for every target it advertises.
pub fn bundled_targets(kind: SdkKind, out_dir: &Path, package: &str, binary: &str) -> Vec<String> {
    match kind {
        SdkKind::TypeScript => Target::ALL
            .iter()
            .filter(|t| {
                out_dir
                    .join("platforms")
                    .join(format!("{package}-{}", t.npm_suffix()))
                    .join("bin")
                    .join(format!("{binary}{}", t.exe_suffix))
                    .is_file()
            })
            .map(|t| t.triple.to_string())
            .collect(),
        SdkKind::Python => {
            let dist = out_dir.join("dist");
            let Ok(entries) = std::fs::read_dir(&dist) else {
                return Vec::new();
            };
            let names: Vec<String> = entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.ends_with(".whl"))
                .collect();
            Target::ALL
                .iter()
                .filter(|t| names.iter().any(|n| n.contains(t.wheel_platform)))
                .map(|t| t.triple.to_string())
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(triple: &str) -> &'static Target {
        Target::find(triple).expect("known target")
    }

    fn targets(triples: &[&str]) -> Vec<&'static Target> {
        triples.iter().map(|t| target(t)).collect()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("soothfast-sdk-build-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn cross_compiled_output_is_looked_for_under_its_triple() {
        let dir = Path::new("/w/target");
        assert_eq!(
            binary_path(dir, "srv", target("x86_64-unknown-linux-gnu"), true),
            Path::new("/w/target/x86_64-unknown-linux-gnu/release/srv")
        );
        assert_eq!(
            binary_path(dir, "srv", target("x86_64-unknown-linux-gnu"), false),
            Path::new("/w/target/x86_64-unknown-linux-gnu/debug/srv")
        );
        // Windows output keeps its extension all the way into the package.
        assert_eq!(
            binary_path(dir, "srv", target("x86_64-pc-windows-msvc"), true),
            Path::new("/w/target/x86_64-pc-windows-msvc/release/srv.exe")
        );
    }

    #[test]
    fn a_manylinux_tag_built_outside_the_container_is_called_out() {
        let warning =
            manylinux_warning_for(&targets(&["x86_64-unknown-linux-gnu"]), false).expect("warned");
        assert!(warning.contains("manylinux_2_17_x86_64"), "{warning}");
    }

    #[test]
    fn inside_the_container_the_tag_is_earned_and_nothing_is_said() {
        assert_eq!(
            manylinux_warning_for(&targets(&["x86_64-unknown-linux-gnu"]), true),
            None
        );
    }

    #[test]
    fn targets_that_make_no_glibc_claim_never_warn() {
        for triple in [
            "x86_64-apple-darwin",
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-linux-musl",
        ] {
            assert_eq!(
                manylinux_warning_for(&targets(&[triple]), false),
                None,
                "{triple}"
            );
        }
    }

    #[test]
    fn npm_staging_is_only_reported_for_binaries_that_landed() {
        let out = scratch("npm");
        let source = out.join("server");
        std::fs::write(&source, b"#!/bin/sh\n").expect("write");
        let items = vec![Built {
            target: target("x86_64-unknown-linux-gnu"),
            binary: source,
        }];

        let staged = stage_npm(&out, "acme-items", "server", &items).expect("stages");
        assert_eq!(staged, ["x86_64-unknown-linux-gnu"]);

        let landed = out.join("platforms/acme-items-linux-x64/bin/server");
        assert!(landed.is_file(), "{}", landed.display());
        assert_eq!(
            bundled_targets(SdkKind::TypeScript, &out, "acme-items", "server"),
            ["x86_64-unknown-linux-gnu"]
        );
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn a_windows_binary_is_staged_under_its_exe_name() {
        let out = scratch("win");
        let source = out.join("server.exe");
        std::fs::write(&source, b"MZ").expect("write");
        let items = vec![Built {
            target: target("x86_64-pc-windows-msvc"),
            binary: source,
        }];
        stage_npm(&out, "acme-items", "server", &items).expect("stages");
        assert!(
            out.join("platforms/acme-items-win32-x64/bin/server.exe")
                .is_file()
        );
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn nothing_staged_means_nothing_bundled() {
        let out = scratch("empty");
        assert!(bundled_targets(SdkKind::TypeScript, &out, "acme-items", "server").is_empty());
        assert!(bundled_targets(SdkKind::Python, &out, "acme-items", "server").is_empty());
        let _ = std::fs::remove_dir_all(&out);
    }

    /// Python coverage is read off the wheel filenames, which is what
    /// `uv publish` will upload.
    #[test]
    fn python_coverage_is_read_from_the_wheel_platform_tags() {
        let out = scratch("wheels");
        std::fs::create_dir_all(out.join("dist")).expect("dist");
        for name in [
            "acme_items-1.2.3-py3-none-manylinux_2_17_x86_64.whl",
            "acme_items-1.2.3-py3-none-macosx_11_0_arm64.whl",
            "acme_items-1.2.3.tar.gz",
        ] {
            std::fs::write(out.join("dist").join(name), b"").expect("write");
        }
        let bundled = bundled_targets(SdkKind::Python, &out, "acme-items", "server");
        assert_eq!(
            bundled,
            ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]
        );
        let _ = std::fs::remove_dir_all(&out);
    }
}
