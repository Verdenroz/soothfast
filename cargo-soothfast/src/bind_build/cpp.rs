use std::path::Path;
use std::process::Command;

use super::cargo_build::cargo;
use super::staging::{artifacts, hush};

/// The C backend's own build, then a syntax-only compile of a generated
/// `target/check.cpp` that includes the header. A missing C++ compiler
/// skips that step rather than failing the cdylib the cargo build already
/// produced.
pub(super) fn cpp(
    glue: &Path,
    targets: &[String],
    release: bool,
    quiet: bool,
) -> Result<Vec<String>, String> {
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
    let Some(header_name) = Path::new(&header).file_name() else {
        return Err(format!("{header}: built header has no file name"));
    };
    let header_name = header_name.to_string_lossy();
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
pub(crate) fn cpp_compiler() -> Option<String> {
    std::env::var("CXX").ok().or_else(|| {
        ["c++", "g++", "clang++"]
            .into_iter()
            .find(|c| Command::new(c).arg("--version").output().is_ok())
            .map(str::to_string)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bind_build::test_support::{copy_dir, lock};
    use soothfast_bind::BindKind;

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
        lock(&glue);

        let artifacts =
            crate::bind_build::run(BindKind::Cpp, &glue, &[], &[], false, false).expect("builds");
        assert!(
            artifacts.iter().any(|a| a.ends_with(".hpp")),
            "no header among {artifacts:?}"
        );
        assert!(
            glue.join("target/check.cpp").exists(),
            "the syntax-only driver was never written"
        );
    }
}
