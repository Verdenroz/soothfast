//! Shelling out to each language's own packaging tool.
//!
//! `maturin` drives its own build through `pyo3-build-config`, which knows
//! which interpreter to link against; `wasm-pack` drives cargo plus
//! wasm-bindgen's post-processor; `napi build` drives cargo plus its own
//! `.node`/`.d.ts`/JS-loader generation. C has no such tool, so that one is a
//! plain `cargo build`: the glue crate already declares both library kinds.
//! Go rides on that same `cargo build`, then verifies the wrapper compiles
//! against it with `go vet`/`go build`. Ruby rides `bundle exec rake
//! compile` (rb_sys's own `cargo build` wrapper) and then `gem build`; both
//! are independent skip points; a machine with neither installed reports
//! both rather than failing on the first. Lua rides that same `cargo
//! build` too, then verifies the wrapper the same way Go does, except the
//! check runs the module rather than compiling one.

use std::path::{Path, PathBuf};
use std::process::Command;

use soothfast_bind::BindKind;
use soothfast_sdk::target::Target;

use crate::sdk_build;

/// Build one entry's package, returning the artifacts it produced. `quiet`
/// silences each tool's own stdout, for a caller (`bind bench`) whose own
/// stdout must carry nothing else.
pub(crate) fn run(
    kind: BindKind,
    glue: &Path,
    targets: &[String],
    release: bool,
    quiet: bool,
) -> Result<Vec<String>, String> {
    match kind {
        BindKind::Python => maturin(glue, targets, release, quiet),
        BindKind::Wasm => wasm_pack(glue, targets, release, quiet),
        BindKind::Node => napi(glue, targets, release, quiet),
        BindKind::CAbi => cargo(glue, targets, release, quiet),
        BindKind::Go => go(glue, targets, release, quiet),
        BindKind::Java => jvm(glue, targets, release, JAVA, quiet),
        BindKind::Kotlin => jvm(glue, targets, release, KOTLIN, quiet),
        BindKind::R => r(glue, targets, quiet),
        BindKind::Ruby => ruby(glue, targets, quiet),
        BindKind::Cpp => cpp(glue, targets, release, quiet),
        BindKind::Lua => lua(glue, targets, release, quiet),
        BindKind::CSharp => csharp(glue, targets, release, quiet),
    }
}

/// Silences a command's own stdout when `quiet`, so a caller building its
/// own stdout protocol (`bind bench`'s table or JSON) sees none of it.
fn hush(cmd: &mut Command, quiet: bool) {
    if quiet {
        cmd.stdout(std::process::Stdio::null());
    }
}

/// The C backend's own build, then `go vet`/`go build` over the wrapper
/// generated against it. A missing `go` toolchain skips the verification
/// rather than failing the cdylib the cargo build already produced.
fn go(glue: &Path, targets: &[String], release: bool, quiet: bool) -> Result<Vec<String>, String> {
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

/// The C backend's own build, then a syntax-only compile of a generated
/// `target/check.cpp` that includes the header. A missing C++ compiler
/// skips that step rather than failing the cdylib the cargo build already
/// produced.
fn cpp(glue: &Path, targets: &[String], release: bool, quiet: bool) -> Result<Vec<String>, String> {
    let mut out = cargo(glue, targets, release, quiet)?;
    let Some(header) = artifacts(glue, &["hpp"])?.into_iter().next() else {
        return Ok(out);
    };
    out.push(header.clone());
    let Some(compiler) = cpp_compiler() else {
        eprintln!(
            "soothfast: no C++ compiler found ($CXX, c++, g++, clang++); \
             skipping wrapper verification"
        );
        return Ok(out);
    };
    let header_name = Path::new(&header)
        .file_name()
        .expect("a built header has a name")
        .to_string_lossy();
    let target = glue.join("target");
    std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
    let check = target.join("check.cpp");
    std::fs::write(&check, format!("#include \"{header_name}\"\n")).map_err(|e| e.to_string())?;

    let mut cmd = Command::new(&compiler);
    cmd.args(["-std=c++20", "-fsyntax-only", "-I"])
        .arg(glue)
        .arg(&check);
    hush(&mut cmd, quiet);
    let status = cmd
        .status()
        .map_err(|e| format!("cannot run {compiler}: {e}"))?;
    if !status.success() {
        return Err(format!("`{compiler} -fsyntax-only` failed"));
    }
    Ok(out)
}

/// `$CXX` first, then the usual PATH names, in the order a consumer's own
/// build would try them.
fn cpp_compiler() -> Option<String> {
    std::env::var("CXX").ok().or_else(|| {
        ["c++", "g++", "clang++"]
            .into_iter()
            .find(|c| Command::new(c).arg("--version").output().is_ok())
            .map(str::to_string)
    })
}

/// The C backend's own build, then a `require` load check through the real
/// LuaJIT resolver: `LUA_PATH` finds the `.lua` module the way a consumer's
/// own `require` would, and `LD_LIBRARY_PATH` finds the cdylib `ffi.load`
/// looks for by its bare name. A missing `luajit`, or a build that produced
/// no host-native library (only cross targets), skips the check rather than
/// failing the cdylib the cargo build already produced.
fn lua(glue: &Path, targets: &[String], release: bool, quiet: bool) -> Result<Vec<String>, String> {
    let out = cargo(glue, targets, release, quiet)?;
    let profile = if release { "release" } else { "debug" };
    let lib_dir = glue.join("target").join(profile);
    if !lib_dir.is_dir() {
        eprintln!(
            "soothfast: no host-native build under target/{profile}; skipping the \
             require load check"
        );
        return Ok(out);
    }
    let module = match lua_module(glue) {
        Ok(module) => module,
        Err(why) => {
            eprintln!("soothfast: skipping the require load check: {why}");
            return Ok(out);
        }
    };
    let mut cmd = Command::new("luajit");
    cmd.arg("-e")
        .arg(format!("require('{module}')"))
        .current_dir(glue)
        .env("LUA_PATH", "./?.lua;;")
        .env("LD_LIBRARY_PATH", &lib_dir);
    hush(&mut cmd, quiet);
    match cmd.status() {
        Ok(status) if status.success() => {}
        Ok(_) => return Err(format!("`luajit -e \"require('{module}')\"` failed")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "soothfast: `luajit` not found — https://luajit.org/download.html; \
                 skipping the require load check"
            );
        }
        Err(e) => return Err(format!("cannot run luajit: {e}")),
    }
    Ok(out)
}

/// The one `.lua` file `bind gen` wrote, as the dotted path `require` names
/// it by: the reverse of the loader's own dot-to-separator conversion.
fn lua_module(glue: &Path) -> Result<String, String> {
    let path = find_lua_file(glue, glue)
        .ok_or_else(|| "no .lua file in the package directory".to_string())?;
    Ok(path
        .with_extension("")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "."))
}

fn find_lua_file(dir: &Path, root: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.filter_map(Result::ok) {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == "target") {
            continue;
        }
        if path.is_dir() {
            if let Some(found) = find_lua_file(&path, root) {
                return Some(found);
            }
        } else if path.extension().is_some_and(|e| e == "lua") {
            return path.strip_prefix(root).ok().map(Path::to_path_buf);
        }
    }
    None
}

/// `R CMD INSTALL` straight from the source directory into a library under
/// the glue's own `target/`, never the user's site library.
///
/// No `R CMD build` tarball here: `src/rust/Cargo.toml` depends on the bound
/// crate by a relative path reaching outside the R package's own tree (the
/// glue sits inside the bound crate's tree, not the other way around), and
/// `R CMD INSTALL` extracts a tarball into its own isolated staging
/// directory first, which has no sibling to satisfy that path. A tarball
/// that cannot be installed is not an artifact worth producing; installing
/// from `.` instead builds in place, where the path still resolves.
///
/// R has no cross-compilation matrix of its own to target, so an R package
/// always builds for the host; `targets` is only ever non-empty here because
/// a `[[bind]]` entry meant for another language shares this call.
fn r(glue: &Path, targets: &[String], quiet: bool) -> Result<Vec<String>, String> {
    if !targets.is_empty() {
        eprintln!(
            "soothfast: ignoring --target for r; an R package always builds \
             for the host"
        );
    }
    let library = glue.join("target/rlib");
    std::fs::create_dir_all(&library).map_err(|e| e.to_string())?;
    let mut cmd = Command::new("R");
    cmd.arg("CMD")
        .arg("INSTALL")
        .arg("--no-multiarch")
        .arg(format!("--library={}", library.display()))
        .arg(".")
        .current_dir(glue);
    hush(&mut cmd, quiet);
    let status = cmd.status().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            "`R` not found — install R (https://www.r-project.org)".to_string()
        }
        _ => format!("cannot run R: {e}"),
    })?;
    if !status.success() {
        return Err("`R CMD INSTALL` failed".to_string());
    }
    Ok(vec![library.display().to_string()])
}

/// One `cargo build` per target, or one untargeted build when none are
/// configured.
///
/// A target whose toolchain is missing is reported and skipped rather than
/// failing the run, the same way the SDK's matrix behaves: a machine rarely
/// carries every cross-linker, and the targets it does carry are still worth
/// building.
fn cargo(
    glue: &Path,
    targets: &[String],
    release: bool,
    quiet: bool,
) -> Result<Vec<String>, String> {
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
        match compile_c(glue, target, release, profile, quiet) {
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
    quiet: bool,
) -> Result<Vec<String>, String> {
    let mut args = vec!["build"];
    if release {
        args.push("--release");
    }
    if let Some(triple) = target {
        args.extend(["--target", triple]);
    }
    let mut cmd = Command::new("cargo");
    cmd.args(&args)
        .current_dir(glue)
        // The glue crate is its own workspace; an inherited CARGO_TARGET_DIR
        // would move its output where the artifact scan below never looks.
        .env_remove("CARGO_TARGET_DIR");
    hush(&mut cmd, quiet);
    let status = cmd.status().map_err(|e| format!("cannot run cargo: {e}"))?;
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

/// One JVM host language's own tools: where its sources live under
/// `src/main`, what compiles them, and what to say when that compiler is
/// missing.
struct JvmLang {
    src_dir: &'static str,
    ext: &'static str,
    compiler: &'static str,
    install_hint: &'static str,
}

const JAVA: JvmLang = JvmLang {
    src_dir: "java",
    ext: "java",
    compiler: "javac",
    install_hint: "install a JDK",
};

const KOTLIN: JvmLang = JvmLang {
    src_dir: "kotlin",
    ext: "kt",
    compiler: "kotlinc",
    install_hint: "install the Kotlin compiler — https://kotlinlang.org/docs/command-line.html",
};

/// `cargo build` of the cdylib, same matrix behavior as C, then the host
/// language's own compiler and `jar` on top. A missing compiler skips only
/// the packaging step: the cdylib the matrix already built is still worth
/// reporting.
fn jvm(
    glue: &Path,
    targets: &[String],
    release: bool,
    lang: JvmLang,
    quiet: bool,
) -> Result<Vec<String>, String> {
    let mut out = cargo(glue, targets, release, quiet)?;
    match package_jar(glue, targets, release, lang, quiet) {
        Ok(jar) => out.push(jar),
        Err(why) => eprintln!("soothfast: skipping jar packaging: {why}"),
    }
    out.sort();
    Ok(out)
}

/// Compiles the host language's sources and stages each target's cdylib
/// under `natives/<os>-<arch>/` inside one jar, the layout a JNI loader
/// expects when a package bundles more than one platform's library.
fn package_jar(
    glue: &Path,
    targets: &[String],
    release: bool,
    lang: JvmLang,
    quiet: bool,
) -> Result<String, String> {
    let mut sources = Vec::new();
    jvm_sources(
        &glue.join("src/main").join(lang.src_dir),
        lang.ext,
        &mut sources,
    );
    if sources.is_empty() {
        return Err(format!(
            "no {} sources under src/main/{}",
            lang.src_dir, lang.src_dir
        ));
    }

    let classes = glue.join("target/classes");
    let mut compile = Command::new(lang.compiler);
    compile.arg("-d").arg(&classes).args(&sources);
    hush(&mut compile, quiet);
    let status = compile.status().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            format!("`{}` not found — {}", lang.compiler, lang.install_hint)
        }
        _ => format!("cannot run {}: {e}", lang.compiler),
    })?;
    if !status.success() {
        return Err(format!("`{}` failed", lang.compiler));
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
    hush(&mut jar_cmd, quiet);
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

/// Every file with extension `ext` under `dir`, however deep the package
/// path nests it.
fn jvm_sources(dir: &Path, ext: &str, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            jvm_sources(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

/// The C backend's own build (same matrix behavior as Go and the JVM
/// backends), staged under `runtimes/<rid>/native/` for `Native.cs`'s own
/// resolver to find, then `dotnet build -c Release` over the class library
/// referencing it. A missing `dotnet` skips only that build: the cdylib the
/// matrix already built is still worth reporting.
fn csharp(
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
        let name = lib.file_name().expect("a built library has a name");
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
            "`dotnet` not found — install the .NET SDK (https://dotnet.microsoft.com/download)"
                .to_string()
        }
        _ => format!("cannot run dotnet: {e}"),
    })?;
    if !status.success() {
        return Err("`dotnet build` failed".to_string());
    }
    artifacts(&glue.join("bin").join(config).join("net8.0"), &["dll"])
}

/// `bundle exec rake compile`, then `gem build`, each reported and skipped on
/// its own rather than one failure hiding the other: a machine with neither
/// `bundle` nor `gem` installed should say so about both.
fn ruby(glue: &Path, targets: &[String], quiet: bool) -> Result<Vec<String>, String> {
    if !targets.is_empty() {
        eprintln!(
            "soothfast: ignoring --target for ruby; there is no cross-compiling \
             matrix wired up here, so this always builds for the host"
        );
    }
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    match rake_compile(glue, quiet) {
        Ok(built) => out.extend(built),
        Err(why) => skipped.push(format!("rake compile: {why}")),
    }
    match gem_build(glue, quiet) {
        Ok(gem) => out.push(gem),
        Err(why) => skipped.push(format!("gem build: {why}")),
    }
    for line in &skipped {
        eprintln!("soothfast: skipping {line}");
    }
    if out.is_empty() {
        return Err(format!("nothing built:\n  {}", skipped.join("\n  ")));
    }
    Ok(out)
}

/// `RbSys::ExtensionTask` stages the compiled extension under `lib/<module>/`,
/// the same directory the generated `Rakefile`'s `ext.lib_dir` names.
fn rake_compile(glue: &Path, quiet: bool) -> Result<Vec<String>, String> {
    let mut cmd = Command::new("bundle");
    cmd.args(["exec", "rake", "compile"]).current_dir(glue);
    hush(&mut cmd, quiet);
    let status = cmd.status().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "`bundle` not found — gem install bundler".to_string(),
        _ => format!("cannot run bundle: {e}"),
    })?;
    if !status.success() {
        return Err("`bundle exec rake compile` failed".to_string());
    }
    let module = ext_module(glue)?;
    artifacts(&glue.join("lib").join(module), &["so", "bundle", "dll"])
}

/// The one directory under `ext/`, named after the gem's requirable path.
fn ext_module(glue: &Path) -> Result<String, String> {
    let dir = glue.join("ext");
    std::fs::read_dir(&dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .find(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .ok_or_else(|| format!("no extension directory under {}", dir.display()))
}

/// `gem build` packages a source gem straight from the `.gemspec`; it needs
/// no compiled extension of its own, since `spec.extensions` builds one at
/// install time.
fn gem_build(glue: &Path, quiet: bool) -> Result<String, String> {
    let gemspec = std::fs::read_dir(glue)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().to_string())
        .find(|name| name.ends_with(".gemspec"))
        .ok_or_else(|| "no .gemspec in the package directory".to_string())?;
    let mut cmd = Command::new("gem");
    cmd.args(["build", &gemspec]).current_dir(glue);
    hush(&mut cmd, quiet);
    let status = cmd.status().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "`gem` not found — install a Ruby toolchain".to_string(),
        _ => format!("cannot run gem: {e}"),
    })?;
    if !status.success() {
        return Err("`gem build` failed".to_string());
    }
    artifacts(glue, &["gem"])?
        .into_iter()
        .next()
        .ok_or_else(|| "built, but no .gem landed in the package directory".to_string())
}

/// One `wasm-pack build`, whatever the configured targets say.
///
/// A `.wasm` has no os/cpu/libc axis, so the triples a Python package needs
/// mean nothing here and naming any is a mistake worth reporting.
fn wasm_pack(
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
    let mut cmd = Command::new("wasm-pack");
    cmd.args(&args)
        .current_dir(glue)
        // wasm-pack shells out to cargo itself; same CARGO_TARGET_DIR
        // reasoning as the C backend and maturin.
        .env_remove("CARGO_TARGET_DIR");
    hush(&mut cmd, quiet);
    let status = cmd.status().map_err(|e| match e.kind() {
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
fn napi(
    glue: &Path,
    targets: &[String],
    release: bool,
    quiet: bool,
) -> Result<Vec<String>, String> {
    if !glue.join("node_modules").exists() {
        npm_install(glue, quiet)?;
    }

    let mut failures = Vec::new();
    if targets.is_empty() {
        napi_build_once(glue, None, release, quiet)?;
    } else {
        for target in targets {
            if let Err(e) = napi_build_once(glue, Some(target), release, quiet) {
                failures.push(format!("{target}: {e}"));
            }
        }
    }
    for failure in &failures {
        eprintln!("soothfast: skipped {failure}");
    }
    artifacts(glue, &["node"])
}

fn npm_install(glue: &Path, quiet: bool) -> Result<(), String> {
    let mut cmd = Command::new("npm");
    cmd.arg("install").current_dir(glue);
    hush(&mut cmd, quiet);
    let status = cmd.status().map_err(|e| match e.kind() {
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

fn napi_build_once(
    glue: &Path,
    target: Option<&str>,
    release: bool,
    quiet: bool,
) -> Result<(), String> {
    // `--platform` makes `napi build` emit the JS loader package.json's
    // `main` names; without it only the `.node` file lands.
    let mut args = vec!["napi", "build", "--platform"];
    if release {
        args.push("--release");
    }
    if let Some(triple) = target {
        args.extend(["--target", triple]);
    }
    let mut cmd = Command::new("npx");
    cmd.args(&args)
        .current_dir(glue)
        // napi build shells out to cargo itself; same CARGO_TARGET_DIR
        // reasoning as the C backend and maturin.
        .env_remove("CARGO_TARGET_DIR");
    hush(&mut cmd, quiet);
    let status = cmd.status().map_err(|e| match e.kind() {
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
fn maturin(
    glue: &Path,
    targets: &[String],
    release: bool,
    quiet: bool,
) -> Result<Vec<String>, String> {
    let resolved = Target::matrix(targets)?;
    if !targets.is_empty()
        && let Some(warning) = sdk_build::manylinux_warning(&resolved)
    {
        eprintln!("soothfast: {warning}");
    }

    let mut failures = Vec::new();
    if targets.is_empty() {
        maturin_once(glue, None, release, quiet)?;
    } else {
        for target in &resolved {
            if let Err(e) = maturin_once(glue, Some(target.triple), release, quiet) {
                failures.push(format!("{}: {e}", target.triple));
            }
        }
    }
    for failure in &failures {
        eprintln!("soothfast: skipped {failure}");
    }
    artifacts(&glue.join("target/wheels"), &["whl"])
}

fn maturin_once(
    glue: &Path,
    triple: Option<&str>,
    release: bool,
    quiet: bool,
) -> Result<(), String> {
    let mut args = vec!["build"];
    if release {
        args.push("--release");
    }
    if let Some(triple) = triple {
        args.extend(["--target", triple]);
    }
    let mut cmd = Command::new("maturin");
    cmd.args(&args)
        .current_dir(glue)
        // Same reason as the C backend's cargo build: an inherited
        // CARGO_TARGET_DIR would move the wheel where `artifacts` below
        // never looks.
        .env_remove("CARGO_TARGET_DIR");
    hush(&mut cmd, quiet);
    let status = cmd.status().map_err(|e| match e.kind() {
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

        let artifacts = run(BindKind::Java, &glue, &[], false, false).expect("builds");
        assert!(
            artifacts.iter().any(|a| a.ends_with(".jar")),
            "no jar among {artifacts:?}"
        );
    }

    /// Same shape, over the Kotlin golden. `kotlinc` is absent on this
    /// machine, so the assertion is the contract the missing-compiler path
    /// promises rather than a fixed pass/fail: the cdylib still builds
    /// either way, and a jar appears exactly when `kotlinc` is on `PATH`.
    #[test]
    #[ignore = "shells out to cargo, kotlinc and jar"]
    fn kotlin_build_packages_a_jar_exactly_when_kotlinc_is_available() {
        let bind_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../soothfast-bind/tests");
        let scratch = std::env::temp_dir().join(format!(
            "soothfast-bind-kotlin-build-smoke-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        copy_dir(&bind_dir.join("fixture_crate"), &scratch);
        let glue = scratch.join("glue");
        copy_dir(&bind_dir.join("goldens/kotlin"), &glue);

        let artifacts = run(BindKind::Kotlin, &glue, &[], false, false).expect("builds");
        let has_jar = artifacts.iter().any(|a| a.ends_with(".jar"));
        let kotlinc_present = std::process::Command::new("kotlinc")
            .arg("-version")
            .output()
            .is_ok();
        assert_eq!(
            has_jar, kotlinc_present,
            "jar packaging must track kotlinc's presence: {artifacts:?}"
        );
    }

    /// Builds `soothfast-bind`'s own R golden the way `bind build` would, in
    /// the same fixture-crate-at-the-root layout `r_smoke.rs` uses. Ignored:
    /// it shells out to `R CMD INSTALL`.
    #[test]
    #[ignore = "shells out to R CMD INSTALL"]
    fn r_build_installs_a_library() {
        let bind_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../soothfast-bind/tests");
        let scratch = std::env::temp_dir().join(format!(
            "soothfast-bind-r-build-smoke-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        copy_dir(&bind_dir.join("fixture_crate"), &scratch);
        let glue = scratch.join("glue");
        copy_dir(&bind_dir.join("goldens/r"), &glue);

        let artifacts = run(BindKind::R, &glue, &[], false, false).expect("builds");
        assert!(
            artifacts.iter().any(|a| a.ends_with("target/rlib")),
            "no library among {artifacts:?}"
        );
        assert!(
            glue.join("target/rlib/acme.core").is_dir(),
            "package not installed under target/rlib"
        );
    }

    /// Builds `soothfast-bind`'s own C++ golden the way `bind build` would,
    /// in the same fixture-crate-at-the-root layout `cpp_smoke.rs` uses.
    /// Ignored: it shells out to cargo and a C++ compiler.
    #[test]
    #[ignore = "shells out to cargo and a C++ compiler"]
    fn cpp_build_verifies_the_header_by_syntax_only() {
        let bind_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../soothfast-bind/tests");
        let scratch = std::env::temp_dir().join(format!(
            "soothfast-bind-cpp-build-smoke-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        copy_dir(&bind_dir.join("fixture_crate"), &scratch);
        let glue = scratch.join("glue");
        copy_dir(&bind_dir.join("goldens/cpp"), &glue);

        let artifacts = run(BindKind::Cpp, &glue, &[], false, false).expect("builds");
        assert!(
            artifacts.iter().any(|a| a.ends_with(".hpp")),
            "no header among {artifacts:?}"
        );
        assert!(
            glue.join("target/check.cpp").exists(),
            "the syntax-only driver was never written"
        );
    }

    /// Builds `soothfast-bind`'s own Lua golden the way `bind build` would,
    /// in the same fixture-crate-at-the-root layout `lua_smoke.rs` uses.
    /// Ignored: it shells out to cargo and luajit.
    #[test]
    #[ignore = "shells out to cargo and luajit"]
    fn lua_build_passes_the_require_load_check() {
        let bind_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../soothfast-bind/tests");
        let scratch = std::env::temp_dir().join(format!(
            "soothfast-bind-lua-build-smoke-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        copy_dir(&bind_dir.join("fixture_crate"), &scratch);
        let glue = scratch.join("glue");
        copy_dir(&bind_dir.join("goldens/lua"), &glue);

        let artifacts = run(BindKind::Lua, &glue, &[], false, false).expect("builds");
        assert!(
            artifacts
                .iter()
                .any(|a| a.ends_with(".so") || a.ends_with(".dylib")),
            "no cdylib among {artifacts:?}"
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
