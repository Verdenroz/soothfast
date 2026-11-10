//! Writing a target release into the files `scan` read.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use super::scan::{Fix, Pin};
use super::{LOCK, Scanned};

/// A `(package, version)` entry of Cargo.lock.
pub type Locked = (String, String);

/// The commit a release tag names in a repository: `(slug, tag)`.
pub type Resolve<'a> = &'a dyn Fn(&str, &str) -> Option<String>;

/// Moves the repository's Cargo.lock entries to the target.
pub type UpdateLock<'a> = &'a dyn Fn(&Path, &[Locked], &str) -> Result<(), String>;

/// What rewriting needs from outside the repository.
pub struct Tools<'a> {
    pub resolve: Resolve<'a>,
    pub update_lock: UpdateLock<'a>,
}

impl Tools<'_> {
    pub fn real() -> Tools<'static> {
        Tools {
            resolve: &crate::tagref::tag_sha,
            update_lock: &cargo_update,
        }
    }
}

/// Rewrite every stale pin to `target`; the paths of the files that changed.
pub fn rewrite(
    root: &Path,
    scanned: &[Scanned],
    target: &str,
    tools: &Tools,
) -> Result<Vec<String>, String> {
    let commits = commits(scanned, target, tools.resolve)?;
    let mut changed = Vec::new();
    for file in scanned {
        let stale: Vec<&Pin> = file.scan.pins.iter().filter(|p| !p.current).collect();
        if stale.is_empty() {
            continue;
        }
        let path = root.join(&file.path);
        if file.path == LOCK {
            let from: Vec<Locked> = stale
                .iter()
                .filter_map(|p| Some((p.package.clone()?, p.found.clone())))
                .collect();
            (tools.update_lock)(root, &from, target)?;
            let after = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            if after != file.text {
                changed.push(file.path.clone());
            }
            continue;
        }
        let text = apply(&file.text, &stale, target, &commits)?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        changed.push(file.path.clone());
    }
    Ok(changed)
}

fn commits(
    scanned: &[Scanned],
    target: &str,
    resolve: Resolve,
) -> Result<BTreeMap<String, String>, String> {
    let tag = format!("v{target}");
    let mut commits = BTreeMap::new();
    let stale = scanned
        .iter()
        .flat_map(|f| &f.scan.pins)
        .filter(|p| !p.current);
    for pin in stale {
        if let Fix::Commit { slug, .. } = &pin.fix
            && !commits.contains_key(slug)
        {
            let sha =
                resolve(slug, &tag).ok_or_else(|| format!("could not resolve {tag} in {slug}"))?;
            commits.insert(slug.clone(), sha);
        }
    }
    Ok(commits)
}

fn apply(
    text: &str,
    pins: &[&Pin],
    target: &str,
    commits: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut ordered = pins.to_vec();
    ordered.sort_by_key(|p| Reverse(p.span.start));
    let mut out = text.to_string();
    for pin in ordered {
        let new = match &pin.fix {
            Fix::Text(t) => t.clone(),
            Fix::Commit { slug, comment } => {
                let sha = commits
                    .get(slug)
                    .ok_or("a release commit was not resolved")?;
                if *comment {
                    format!("{sha} # v{target}")
                } else {
                    sha.clone()
                }
            }
        };
        out.replace_range(pin.span.clone(), &new);
    }
    Ok(out)
}

fn cargo_update(root: &Path, packages: &[Locked], target: &str) -> Result<(), String> {
    for (name, from) in packages {
        let out = Command::new("cargo")
            .current_dir(root)
            .args(["update", "--package", &format!("{name}@{from}")])
            .args(["--precise", target])
            .output()
            .map_err(|e| format!("cargo update {name}: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!("cargo update {name}: {}", stderr.trim()));
        }
    }
    Ok(())
}
