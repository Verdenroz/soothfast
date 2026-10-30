//! The non-Rust projects a repo tracks, found from `git ls-files`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::invoke;

use super::ecosystem::{Ecosystem, normalize_dir};

/// Directories whose contents are build output or vendored, never a project.
const IGNORED: &[&str] = &[
    "target",
    "build",
    "dist",
    "node_modules",
    ".venv",
    "vendor",
    ".soothfast",
];

/// Each non-Rust project under `root` as its directory (`.` for the root
/// itself) and ecosystem, in directory order.
pub fn projects(root: &Path) -> Vec<(String, Ecosystem)> {
    let listing = invoke::git_in(root, &["ls-files", "-z"]).unwrap_or_default();
    let paths: Vec<&str> = listing.split('\0').filter(|p| !p.is_empty()).collect();
    from_paths(&paths)
}

fn from_paths(paths: &[&str]) -> Vec<(String, Ecosystem)> {
    let tracked: Vec<(&str, &str)> = paths
        .iter()
        .map(|&p| p.rsplit_once('/').unwrap_or((".", p)))
        .filter(|(dir, _)| dir.split('/').all(|part| !IGNORED.contains(&part)))
        .collect();
    let dirs_with = |names: &[&str]| -> Vec<String> {
        let mut dirs: Vec<String> = tracked
            .iter()
            .filter(|(_, name)| names.contains(name))
            .map(|(dir, _)| dir.to_string())
            .collect();
        dirs.sort();
        dirs.dedup();
        dirs
    };

    let settings = dirs_with(&["settings.gradle", "settings.gradle.kts"]);
    let loose: Vec<String> = dirs_with(&["build.gradle", "build.gradle.kts"])
        .into_iter()
        .filter(|dir| !settings.iter().any(|s| contains(s, dir)))
        .collect();
    let gradle = [settings, topmost(loose)].concat();

    let mut found: BTreeMap<String, Ecosystem> = BTreeMap::new();
    let groups = [
        (Ecosystem::Gradle, gradle),
        (Ecosystem::Maven, topmost(dirs_with(&["pom.xml"]))),
        (
            Ecosystem::Python,
            topmost(dirs_with(&["pyproject.toml", "setup.py"])),
        ),
        (Ecosystem::Node, topmost(dirs_with(&["package.json"]))),
    ];
    for (eco, dirs) in groups {
        for dir in dirs {
            if let Ok(dir) = normalize_dir(&dir) {
                found.entry(dir).or_insert(eco);
            }
        }
    }
    found.into_iter().collect()
}

/// Whether `dir` is `parent` or below it.
fn contains(parent: &str, dir: &str) -> bool {
    parent == "." || dir == parent || dir.starts_with(&format!("{parent}/"))
}

/// The directories of `dirs` that are not below another of them.
fn topmost(dirs: Vec<String>) -> Vec<String> {
    dirs.iter()
        .filter(|d| !dirs.iter().any(|p| p != *d && contains(p, d)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::from_paths as found;
    use super::*;
    use Ecosystem::*;

    fn at(dir: &str, eco: Ecosystem) -> (String, Ecosystem) {
        (dir.into(), eco)
    }

    #[test]
    fn a_settings_file_absorbs_the_builds_below_it() {
        let paths = [
            "Cargo.toml",
            "client/settings.gradle.kts",
            "client/build.gradle.kts",
            "client/shared/build.gradle.kts",
            "client/app/build.gradle",
        ];
        assert_eq!(found(&paths), [at("client", Gradle)]);
    }

    #[test]
    fn without_settings_the_topmost_build_file_is_the_project() {
        let paths = ["a/build.gradle", "a/b/build.gradle", "c/build.gradle.kts"];
        assert_eq!(found(&paths), [at("a", Gradle), at("c", Gradle)]);
    }

    #[test]
    fn maven_and_node_take_the_topmost_directory_only() {
        let paths = [
            "pom.xml",
            "mod/pom.xml",
            "web/package.json",
            "web/ui/package.json",
        ];
        assert_eq!(found(&paths), [at(".", Maven), at("web", Node)]);
    }

    #[test]
    fn python_skips_directories_under_another_python_directory() {
        let paths = [
            "py/pyproject.toml",
            "py/sub/setup.py",
            "tools/setup.py",
            "tools/x/pyproject.toml",
        ];
        assert_eq!(found(&paths), [at("py", Python), at("tools", Python)]);
    }

    #[test]
    fn a_pure_node_repo_is_one_root_project() {
        let paths = ["package.json", "src/index.ts", "src/lib/package.json"];
        assert_eq!(found(&paths), [at(".", Node)]);
    }

    #[test]
    fn build_output_and_vendored_directories_are_ignored() {
        let paths = [
            "package.json",
            "node_modules/dep/package.json",
            "web/node_modules/dep/package.json",
            "target/x/pom.xml",
            "build/y/build.gradle",
            "dist/z/setup.py",
            ".venv/lib/pyproject.toml",
            "vendor/v/package.json",
            ".soothfast/w/package.json",
        ];
        assert_eq!(found(&paths), [at(".", Node)]);
    }

    #[test]
    fn one_directory_holds_one_project_by_precedence() {
        let paths = ["package.json", "pom.xml", "pyproject.toml"];
        assert_eq!(found(&paths), [at(".", Maven)]);
    }

    #[test]
    fn only_tracked_files_count() {
        let root = std::env::temp_dir().join(format!("soothfast-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for file in [
            "Cargo.toml",
            "client/settings.gradle.kts",
            "untracked/package.json",
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        invoke::git_in(&root, &["init", "-q"]).unwrap();
        invoke::git_in(&root, &["add", "Cargo.toml", "client"]).unwrap();
        assert_eq!(projects(&root), [at("client", Gradle)]);
    }
}
