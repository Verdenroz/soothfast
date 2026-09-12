//! Shelling out to each language's own packaging tool.
//!
//! `maturin` drives its own build through `pyo3-build-config`, which knows
//! which interpreter to link against; `wasm-pack` drives cargo plus
//! wasm-bindgen's post-processor; `napi build` drives cargo plus its own
//! `.node`/`.d.ts`/JS-loader generation. C has no such tool, so that one is a
//! plain `cargo build`: the glue crate already declares both library kinds.
//! Go rides on that same `cargo build`, then verifies the wrapper compiles
//! against it with `go vet`/`go build`.

use std::path::Path;
use std::process::Command;

use soothfast_bind::BindKind;
use soothfast_sdk::target::Target;

use crate::sdk_build;

/// Build one entry's package, returning the artifacts it produced.
pub(crate) fn run(
    kind: BindKind,
    glue: &Path,
    targets: &[String],
    release: bool,
) -> Result<Vec<String>, String> {
    match kind {
        BindKind::Python => maturin(glue, targets, release),
        BindKind::Wasm => wasm_pack(glue, targets, release),
        BindKind::Node => napi(glue, targets, release),
        BindKind::CAbi => cargo(glue, targets, release),
        BindKind::Go => go(glue, targets, release),
        BindKind::Java => java(glue, targets, release),
    }
}

/// The C backend's own build, then `go vet`/`go build` over the wrapper
/// generated against it. A missing `go` toolchain skips the verification
/// rather than failing the cdylib the cargo build already produced.
fn go(glue: &Path, targets: &[String], release: bool) -> Result<Vec<String>, String> {
    let out = cargo(glue, targets, release)?;
    for args in [["vet", "./..."], ["build", "./..."]] {
        let status = Command::new("go")
            .args(args)
            .current_dir(glue)
            .env("CGO_ENABLED", "1")
            .status();
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

/// One `cargo build` per target, or one untargeted build when none are
/// configured.
///
/// A target whose toolchain is missing is reported and skipped rather than
/// failing the run, the same way the SDK's matrix behaves: a machine rarely
/// carries every cross-linker, and the targets it does carry are still worth
/// building.
fn cargo(glue: &Path, targets: &[String], release: bool) -> Result<Vec<String>, String> {
    let profile = match release {
        true => "release",
        false => "debug",
    };
    let wanted: Vec<Option<&str>> = match targets.is_empty() {
        true => vec![None],
        false => targets.iter().map(|t| Some(t.as_str())).collect(),
    };
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for target in wanted {
        match compile_c(glue, target, release, profile) {
            Ok(found) => out.extend(found),
            Err(why) => skipped.push(format!("{}: {why}", target.unwrap_or("host"))),
        }
    }
    for line in &skipped {
        eprintln!("soothfast: skipping {line}");
    }
    if out.is_empty() {
        return Err(format!("no target built:\n  {}", skipped.join("\n  ")));
    }
    // The header describes every target, so it ships alongside all of them.
    out.extend(header(glue));
    out.sort();
    Ok(out)
}

fn compile_c(
    glue: &Path,
    target: Option<&str>,
    release: bool,
    profile: &str,
) -> Result<Vec<String>, String> {
    let mut args = vec!["build"];
    if release {
        args.push("--release");
    }
    if let Some(triple) = target {
        args.extend(["--target", triple]);
    }
    let status = Command::new("cargo")
        .args(&args)
        .current_dir(glue)
        // The glue crate is its own workspace; an inherited CARGO_TARGET_DIR
        // would move its output where the artifact scan below never looks.
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err(match target {
            Some(triple) => format!(
                "cargo build failed — is the target installed? \
                 (rustup target add {triple})"
            ),
            None => "cargo build failed".to_string(),
        });
    }
    let dir = match target {
        Some(triple) => glue.join("target").join(triple).join(profile),
        None => glue.join("target").join(profile),
    };
    let found = artifacts(&dir, &["so", "dylib", "dll", "a", "lib"])?;
    match found.is_empty() {
        true => Err(format!("built, but nothing landed in {}", dir.display())),
        false => Ok(found),
    }
}

/// The generated header, which a C consumer needs alongside the library.
fn header(glue: &Path) -> Vec<String> {
    artifacts(glue, &["h"]).unwrap_or_default()
}

/// `cargo build` of the cdylib, same matrix behavior as C, then `javac` and
/// `jar` on top. A missing JDK tool skips only the packaging step: the
/// cdylib the matrix already built is still worth reporting.
fn java(glue: &Path, targets: &[String], release: bool) -> Result<Vec<String>, String> {
    let mut out = cargo(glue, targets, release)?;
    match package_jar(glue, targets, release) {
        Ok(jar) => out.push(jar),
        Err(why) => eprintln!("soothfast: skipping jar packaging: {why}"),
    }
    out.sort();
    Ok(out)
}

/// Compiles the Java sources and stages each target's cdylib under
/// `natives/<os>-<arch>/` inside one jar, the layout a JNI loader expects
/// when a package bundles more than one platform's library.
fn package_jar(glue: &Path, targets: &[String], release: bool) -> Result<String, String> {
    let mut sources = Vec::new();
    java_sources(&glue.join("src/main/java"), &mut sources);
    if sources.is_empty() {
        return Err("no Java sources under src/main/java".to_string());
    }

    let classes = glue.join("target/classes");
    let status = Command::new("javac")
        .arg("-d")
        .arg(&classes)
        .args(&sources)
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "`javac` not found — install a JDK".to_string(),
            _ => format!("cannot run javac: {e}"),
        })?;
    if !status.success() {
        return Err("`javac` failed".to_string());
    }

    let staging = glue.join("target/jar-staging");
    let _ = std::fs::remove_dir_all(&staging);
    stage_natives(glue, targets, release, &staging)?;

    let name = lib_name(glue)?;
    let jar_path = glue.join("target").join(format!("{name}.jar"));
    let mut jar_cmd = Command::new("jar");
    jar_cmd.arg("--create").arg("--file").arg(&jar_path);
    jar_cmd.arg("-C").arg(&classes).arg(".");
    if staging.join("natives").is_dir() {
        jar_cmd.arg("-C").arg(&staging).arg("natives");
    }
    let status = jar_cmd.status().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "`jar` not found — install a JDK".to_string(),
        _ => format!("cannot run jar: {e}"),
    })?;
    if !status.success() {
        return Err("`jar` failed".to_string());
    }
    Ok(jar_path.display().to_string())
}

/// Copies each already-built target's cdylib into `natives/<os>-<arch>/`.
/// Skips a target whose build failed rather than re-reporting it: `cargo`
/// already did.
fn stage_natives(
    glue: &Path,
    targets: &[String],
    release: bool,
    staging: &Path,
) -> Result<(), String> {
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
        let arch = match target {
            Some(t) => soothfast_bind::java_native_dir(t.triple),
            None => format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        };
        let arch_dir = staging.join("natives").join(arch);
        std::fs::create_dir_all(&arch_dir).map_err(|e| e.to_string())?;
        let name = lib.file_name().expect("a built library has a name");
        std::fs::copy(&lib, arch_dir.join(name)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn cdylib_in(dir: &Path) -> Option<std::path::PathBuf> {
    artifacts(dir, &["so", "dylib", "dll"])
        .ok()
        .and_then(|found| found.into_iter().next())
        .map(std::path::PathBuf::from)
}

/// The `[lib] name` the generated `Cargo.toml` declares, which is also what
/// `System.loadLibrary` looks the native library up by.
fn lib_name(glue: &Path) -> Result<String, String> {
    let toml = std::fs::read_to_string(glue.join("Cargo.toml")).map_err(|e| e.to_string())?;
    let mut in_lib = false;
    for line in toml.lines() {
        let line = line.trim();
        if let Some(section) = line.strip_prefix('[') {
            in_lib = section.trim_end_matches(']') == "lib";
            continue;
        }
        if in_lib
            && let Some(("name", value)) = line.split_once('=').map(|(k, v)| (k.trim(), v.trim()))
        {
            return Ok(value.trim_matches('"').to_string());
        }
    }
    Err("no [lib] name in Cargo.toml".to_string())
}

/// Every `.java` file under `dir`, however deep the package path nests it.
fn java_sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            java_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "java") {
            out.push(path);
        }
    }
}

/// One `wasm-pack build`, whatever the configured targets say.
///
/// A `.wasm` has no os/cpu/libc axis, so the triples a Python package needs
/// mean nothing here and naming any is a mistake worth reporting.
fn wasm_pack(glue: &Path, targets: &[String], release: bool) -> Result<Vec<String>, String> {
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
    let status = Command::new("wasm-pack")
        .args(&args)
        .current_dir(glue)
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "`wasm-pack` not found — cargo install wasm-pack".to_string()
            }
            _ => format!("cannot run wasm-pack: {e}"),
        })?;
    if !status.success() {
        return Err(format!("`wasm-pack {}` failed", args.join(" ")));
    }
    artifacts(&glue.join("pkg"), &["wasm", "js", "ts"])
}

/// `npm install` once, then one `napi build` per target, or one untargeted
/// build when none are configured.
///
/// A target whose toolchain is missing is reported and skipped, the same
/// posture as `maturin`'s matrix: a machine rarely carries every
/// cross-linker.
fn napi(glue: &Path, targets: &[String], release: bool) -> Result<Vec<String>, String> {
    if !glue.join("node_modules").exists() {
        npm_install(glue)?;
    }

    let mut failures = Vec::new();
    if targets.is_empty() {
        napi_build_once(glue, None, release)?;
    } else {
        for target in targets {
            if let Err(e) = napi_build_once(glue, Some(target), release) {
                failures.push(format!("{target}: {e}"));
            }
        }
    }
    for failure in &failures {
        eprintln!("soothfast: skipped {failure}");
    }
    artifacts(glue, &["node"])
}

fn npm_install(glue: &Path) -> Result<(), String> {
    let status = Command::new("npm")
        .arg("install")
        .current_dir(glue)
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "`npm` not found — install Node.js (https://nodejs.org)".to_string()
            }
            _ => format!("cannot run npm: {e}"),
        })?;
    match status.success() {
        true => Ok(()),
        false => Err("`npm install` failed".to_string()),
    }
}

fn napi_build_once(glue: &Path, target: Option<&str>, release: bool) -> Result<(), String> {
    // `--platform` makes `napi build` emit the JS loader package.json's
    // `main` names; without it only the `.node` file lands.
    let mut args = vec!["napi", "build", "--platform"];
    if release {
        args.push("--release");
    }
    if let Some(triple) = target {
        args.extend(["--target", triple]);
    }
    let status = Command::new("npx")
        .args(&args)
        .current_dir(glue)
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "`npm`/`npx` not found — install Node.js (https://nodejs.org)".to_string()
            }
            _ => format!("cannot run npx: {e}"),
        })?;
    match status.success() {
        true => Ok(()),
        false => Err(format!("`npx {}` failed", args.join(" "))),
    }
}

/// One `maturin build` per target, or one untargeted build when none are
/// configured.
///
/// A target whose toolchain is missing is reported and skipped: a partial
/// matrix is a normal local outcome, and CI builds the full one.
fn maturin(glue: &Path, targets: &[String], release: bool) -> Result<Vec<String>, String> {
    let resolved = Target::matrix(targets)?;
    if !targets.is_empty()
        && let Some(warning) = sdk_build::manylinux_warning(&resolved)
    {
        eprintln!("soothfast: {warning}");
    }

    let mut failures = Vec::new();
    if targets.is_empty() {
        maturin_once(glue, None, release)?;
    } else {
        for target in &resolved {
            if let Err(e) = maturin_once(glue, Some(target.triple), release) {
                failures.push(format!("{}: {e}", target.triple));
            }
        }
    }
    for failure in &failures {
        eprintln!("soothfast: skipped {failure}");
    }
    artifacts(&glue.join("target/wheels"), &["whl"])
}

fn maturin_once(glue: &Path, triple: Option<&str>, release: bool) -> Result<(), String> {
    let mut args = vec!["build"];
    if release {
        args.push("--release");
    }
    if let Some(triple) = triple {
        args.extend(["--target", triple]);
    }
    let status = Command::new("maturin")
        .args(&args)
        .current_dir(glue)
        .status()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                "`maturin` not found — pip install maturin, or uv tool install maturin".to_string()
            }
            _ => format!("cannot run maturin: {e}"),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`maturin {}` failed", args.join(" ")))
    }
}

/// What the build tool left behind. Reading the directory rather than
/// parsing the tool's output keeps this working across its release notes.
fn artifacts(dir: &Path, extensions: &[&str]) -> Result<Vec<String>, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Err(format!("nothing built in {}", dir.display()));
    };
    let mut out: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| extensions.iter().any(|e| x == *e))
        })
        .map(|p| p.display().to_string())
        .collect();
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds `soothfast-bind`'s own Java golden the way `bind build` would,
    /// in the same fixture-crate-at-the-root layout `java_smoke.rs` uses.
    /// Ignored: it shells out to cargo, javac and jar.
    #[test]
    #[ignore = "shells out to cargo, javac and jar"]
    fn java_build_produces_a_jar() {
        let bind_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../soothfast-bind/tests");
        let scratch =
            std::env::temp_dir().join(format!("soothfast-bind-build-smoke-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        copy_dir(&bind_dir.join("fixture_crate"), &scratch);
        let glue = scratch.join("glue");
        copy_dir(&bind_dir.join("goldens/java"), &glue);

        let artifacts = run(BindKind::Java, &glue, &[], false).expect("builds");
        assert!(
            artifacts.iter().any(|a| a.ends_with(".jar")),
            "no jar among {artifacts:?}"
        );
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
}
