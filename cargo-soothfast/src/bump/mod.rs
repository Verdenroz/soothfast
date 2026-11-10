//! `cargo soothfast bump`: move every soothfast pin in a repository to one
//! release, or with `--check` report the pins that are behind it.

mod scan;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::invoke;

use scan::Scan;

/// Directories whose manifests belong to someone else.
const SKIPPED: &[&str] = &["target", "node_modules", "vendor"];

const WORKFLOWS: &str = ".github/workflows";

/// A file with the pins it holds.
struct Scanned {
    path: String,
    scan: Scan,
}

pub fn run(args: &[String]) -> i32 {
    let mut check = false;
    let mut target = env!("CARGO_PKG_VERSION").to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--check" => check = true,
            "--to" => match it.next().map(|v| v.trim_start_matches('v')) {
                Some(v) if scan::is_version(v) => target = v.to_string(),
                _ => return crate::arg_err("--to needs a release version like 0.5.7"),
            },
            other => return crate::arg_err(&format!("unknown bump arg {other:?}")),
        }
    }
    let scanned = match scan_repo(&invoke::project_root(), &target) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("soothfast: bump: {e}");
            return 2;
        }
    };
    if !check {
        return crate::arg_err("bump without --check is not available yet");
    }
    report(&scanned, &target)
}

fn report(scanned: &[Scanned], target: &str) -> i32 {
    let (lines, stale) = findings(scanned, target);
    for line in lines {
        println!("{line}");
    }
    i32::from(stale)
}

/// A line per stale or unrecognised pin, and whether any pin is stale.
fn findings(scanned: &[Scanned], target: &str) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut stale = false;
    for file in scanned {
        for pin in file.scan.pins.iter().filter(|p| !p.current) {
            lines.push(format!(
                "{}:{}  found {}, want {target}",
                file.path, pin.line, pin.found
            ));
            stale = true;
        }
        for line in &file.scan.unrecognised {
            lines.push(format!(
                "{}:{}  unrecognised cargo-soothfast pin: {}",
                file.path, line.line, line.text
            ));
        }
    }
    (lines, stale)
}

fn scan_repo(root: &Path, target: &str) -> Result<Vec<Scanned>, String> {
    let read =
        |path: &Path| std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()));
    let rel = |path: &Path| {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    };
    let mut files = Vec::new();
    let mut packages = BTreeSet::new();
    for path in manifests(root) {
        let scan = scan::cargo(&read(&path)?, target);
        packages.extend(scan.pins.iter().filter_map(|p| p.package.clone()));
        files.push(Scanned {
            path: rel(&path),
            scan,
        });
    }
    if let Ok(text) = std::fs::read_to_string(root.join("Cargo.lock")) {
        files.push(Scanned {
            path: "Cargo.lock".into(),
            scan: Scan {
                pins: scan::lock(&text, &packages, target),
                ..Scan::default()
            },
        });
    }
    for path in workflows(root) {
        files.push(Scanned {
            path: rel(&path),
            scan: scan::workflow(&read(&path)?, target),
        });
    }
    Ok(files)
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    paths
}

fn manifests(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for path in sorted_entries(dir) {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && !SKIPPED.contains(&name) {
                found.extend(manifests(&path));
            }
        } else if name == "Cargo.toml" {
            found.push(path);
        }
    }
    found
}

fn workflows(root: &Path) -> Vec<PathBuf> {
    sorted_entries(&root.join(WORKFLOWS))
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
        .collect()
}

#[cfg(test)]
mod tests;
