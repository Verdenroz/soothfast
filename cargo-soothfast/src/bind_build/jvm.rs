use std::path::{Path, PathBuf};
use std::process::Command;

use soothfast_sdk::target::Target;

use super::cargo_build::cargo;
use super::staging::{cdylib_in, hush};

/// One JVM host language's own tools: where its sources live under
/// `src/main`, what compiles them, and what to say when that compiler is
/// missing.
pub(super) struct JvmLang {
    src_dir: &'static str,
    ext: &'static str,
    compiler: &'static str,
    install_hint: &'static str,
}

pub(super) const JAVA: JvmLang = JvmLang {
    src_dir: "java",
    ext: "java",
    compiler: "javac",
    install_hint: "install a JDK",
};

pub(super) const KOTLIN: JvmLang = JvmLang {
    src_dir: "kotlin",
    ext: "kt",
    compiler: "kotlinc",
    install_hint: "install the Kotlin compiler — https://kotlinlang.org/docs/command-line.html",
};

/// `cargo build` of the cdylib, same matrix behavior as C, then the host
/// language's own compiler and `jar` on top. A missing compiler skips only
/// the packaging step: the cdylib the matrix already built is still worth
/// reporting.
pub(super) fn jvm(
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
    let classes = compile_classes(glue, lang, quiet)?;

    let staging = glue.join("target/jar-staging");
    let _ = std::fs::remove_dir_all(&staging);
    stage_natives(glue, targets, release, &staging)?;

    create_jar(glue, &classes, &staging, quiet)
}

fn compile_classes(glue: &Path, lang: JvmLang, quiet: bool) -> Result<PathBuf, String> {
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
    Ok(classes)
}

fn create_jar(glue: &Path, classes: &Path, staging: &Path, quiet: bool) -> Result<String, String> {
    let name = lib_name(glue)?;
    let jar_path = glue.join("target").join(format!("{name}.jar"));
    let mut jar_cmd = Command::new("jar");
    jar_cmd.arg("--create").arg("--file").arg(&jar_path);
    jar_cmd.arg("-C").arg(classes).arg(".");
    if staging.join("natives").is_dir() {
        jar_cmd.arg("-C").arg(staging).arg("natives");
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
        let Some(name) = lib.file_name() else {
            return Err(format!("{}: built library has no file name", lib.display()));
        };
        std::fs::copy(&lib, arch_dir.join(name)).map_err(|e| e.to_string())?;
    }
    Ok(())
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
fn jvm_sources(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bind_build::test_support::{copy_dir, lock};
    use soothfast_bind::BindKind;

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
        lock(&glue);

        let artifacts =
            crate::bind_build::run(BindKind::Java, &glue, &[], false, false).expect("builds");
        assert!(
            artifacts.iter().any(|a| a.ends_with(".jar")),
            "no jar among {artifacts:?}"
        );
    }

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
        lock(&glue);

        let artifacts =
            crate::bind_build::run(BindKind::Kotlin, &glue, &[], false, false).expect("builds");
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
}
