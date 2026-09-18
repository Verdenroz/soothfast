use std::path::{Path, PathBuf};
use std::process::Command;

/// Silences a command's own stdout when `quiet`, so a caller building its
/// own stdout protocol (`bind bench`'s table or JSON) sees none of it.
pub(super) fn hush(cmd: &mut Command, quiet: bool) {
    if quiet {
        cmd.stdout(std::process::Stdio::null());
    }
}

/// What the build tool left behind. Reading the directory rather than
/// parsing the tool's output keeps this working across its release notes.
pub(super) fn artifacts(dir: &Path, extensions: &[&str]) -> Result<Vec<String>, String> {
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

/// Removes previously built files matching `extensions` from `dir`, so a
/// later scan of it reports only what this run produced.
pub(super) fn clear_artifacts(dir: &Path, extensions: &[&str]) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.filter_map(Result::ok).map(|e| e.path()) {
        if path
            .extension()
            .is_some_and(|x| extensions.iter().any(|e| x == *e))
        {
            let _ = std::fs::remove_file(&path);
        }
    }
}

pub(super) fn cdylib_in(dir: &Path) -> Option<PathBuf> {
    artifacts(dir, &["so", "dylib", "dll"])
        .ok()
        .and_then(|found| found.into_iter().next())
        .map(PathBuf::from)
}
