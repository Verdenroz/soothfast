use std::path::Path;
use std::process::Command;

use soothfast_sdk::target::Target;

use super::cargo_build::cargo;
use super::staging::{artifacts, cdylib_in, hush};

/// The C backend's own build (same matrix behavior as Go and the JVM
/// backends), staged under `runtimes/<rid>/native/` for `Native.cs`'s own
/// resolver to find, then `dotnet build -c Release` over the class library
/// referencing it. A missing `dotnet` skips only that build: the cdylib the
/// matrix already built is still worth reporting.
pub(super) fn csharp(
    glue: &Path,
    targets: &[String],
    release: bool,
    quiet: bool,
) -> Result<Vec<String>, String> {
    let mut out = cargo(glue, targets, release, quiet)?;
    stage_dotnet_natives(glue, targets, release)?;
    match dotnet_build(glue, release, quiet) {
        Ok(built) => out.extend(built),
        Err(why) => eprintln!("soothfast: skipping dotnet build: {why}"),
    }
    out.sort();
    Ok(out)
}

/// Copies each already-built target's cdylib into `runtimes/<rid>/native/`,
/// the layout `Native.cs`'s `SetDllImportResolver` callback probes.
fn stage_dotnet_natives(glue: &Path, targets: &[String], release: bool) -> Result<(), String> {
    let profile = if release { "release" } else { "debug" };
    let resolved = Target::matrix(targets)?;
    let wanted: Vec<Option<&Target>> = match targets.is_empty() {
        true => vec![None],
        false => resolved.iter().copied().map(Some).collect(),
    };
    for target in wanted {
        let dir = match target {
            Some(t) => glue.join("target").join(t.triple).join(profile),
            None => glue.join("target").join(profile),
        };
        let Some(lib) = cdylib_in(&dir) else {
            continue;
        };
        let rid = match target {
            Some(t) => soothfast_bind::dotnet_rid(t.triple),
            None => host_rid(),
        };
        let native_dir = glue.join("runtimes").join(rid).join("native");
        std::fs::create_dir_all(&native_dir).map_err(|e| e.to_string())?;
        let Some(name) = lib.file_name() else {
            return Err(format!("{}: built library has no file name", lib.display()));
        };
        std::fs::copy(&lib, native_dir.join(name)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// This host's own .NET runtime identifier, for an untargeted build.
fn host_rid() -> String {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        _ => "x64",
    };
    let os = match std::env::consts::OS {
        "macos" => "osx",
        "windows" => "win",
        _ => "linux",
    };
    format!("{os}-{arch}")
}

fn dotnet_build(glue: &Path, release: bool, quiet: bool) -> Result<Vec<String>, String> {
    let mut args = vec!["build"];
    let config = if release { "Release" } else { "Debug" };
    if release {
        args.extend(["-c", config]);
    }
    let mut cmd = Command::new("dotnet");
    cmd.args(&args).current_dir(glue);
    hush(&mut cmd, quiet);
    let status = cmd.status().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            "`dotnet` not found: install the .NET SDK (https://dotnet.microsoft.com/download)"
                .to_string()
        }
        _ => format!("cannot run dotnet: {e}"),
    })?;
    if !status.success() {
        return Err("`dotnet build` failed".to_string());
    }
    artifacts(&glue.join("bin").join(config).join("net8.0"), &["dll"])
}
