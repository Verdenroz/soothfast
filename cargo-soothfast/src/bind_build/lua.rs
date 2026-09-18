use std::path::{Path, PathBuf};
use std::process::Command;

use super::cargo_build::cargo;
use super::staging::hush;

/// The C backend's own build, then a `require` load check through the real
/// LuaJIT resolver: `LUA_PATH` finds the `.lua` module the way a consumer's
/// own `require` would, and `LD_LIBRARY_PATH` finds the cdylib `ffi.load`
/// looks for by its bare name. A missing `luajit`, or a build that produced
/// no host-native library (only cross targets), skips the check rather than
/// failing the cdylib the cargo build already produced.
pub(super) fn lua(
    glue: &Path,
    targets: &[String],
    release: bool,
    quiet: bool,
) -> Result<Vec<String>, String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bind_build::test_support::{copy_dir, lock};
    use soothfast_bind::BindKind;

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
        lock(&glue);

        let artifacts =
            crate::bind_build::run(BindKind::Lua, &glue, &[], false, false).expect("builds");
        assert!(
            artifacts
                .iter()
                .any(|a| a.ends_with(".so") || a.ends_with(".dylib")),
            "no cdylib among {artifacts:?}"
        );
    }
}
