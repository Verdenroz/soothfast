use std::path::Path;
use std::process::Command;

use super::staging::hush;

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
pub(super) fn r(glue: &Path, targets: &[String], quiet: bool) -> Result<Vec<String>, String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bind_build::test_support::copy_dir;
    use soothfast_bind::BindKind;

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

        let artifacts =
            crate::bind_build::run(BindKind::R, &glue, &[], false, false).expect("builds");
        assert!(
            artifacts.iter().any(|a| a.ends_with("target/rlib")),
            "no library among {artifacts:?}"
        );
        assert!(
            glue.join("target/rlib/acme.core").is_dir(),
            "package not installed under target/rlib"
        );
    }
}
