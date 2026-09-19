use std::path::Path;
use std::process::Command;

use super::staging::{artifacts, hush};

/// `bundle exec rake compile`, then `gem build`, each reported and skipped on
/// its own rather than one failure hiding the other: a machine with neither
/// `bundle` nor `gem` installed should say so about both.
pub(super) fn ruby(glue: &Path, targets: &[String], quiet: bool) -> Result<Vec<String>, String> {
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
        std::io::ErrorKind::NotFound => "`bundle` not found: gem install bundler".to_string(),
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
        std::io::ErrorKind::NotFound => "`gem` not found: install a Ruby toolchain".to_string(),
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
