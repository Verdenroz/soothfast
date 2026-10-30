//! The non-Rust projects a repo tracks, found from `git ls-files`.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

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

/// A project found from its build file. `harness` is whether the project's
/// own manifests name a benchmark harness; a root `package.json` that only
/// holds tooling has none.
#[derive(Debug, PartialEq)]
pub struct Found {
    pub dir: String,
    pub ecosystem: Ecosystem,
    pub harness: bool,
}

/// Each non-Rust project under `root`, in directory order. `.` is the root
/// itself.
pub fn projects(root: &Path) -> Vec<Found> {
    let listing = invoke::git_in(root, &["ls-files", "-z"]).unwrap_or_default();
    let paths: Vec<&str> = listing.split('\0').filter(|p| !p.is_empty()).collect();
    from_paths(&paths, &|path| {
        std::fs::read_to_string(root.join(path)).unwrap_or_default()
    })
}

fn from_paths(paths: &[&str], read: &dyn Fn(&str) -> String) -> Vec<Found> {
    let kept: Vec<&str> = paths
        .iter()
        .copied()
        .filter(|p| parent(p).split('/').all(|part| !IGNORED.contains(&part)))
        .collect();
    let dirs_with = |names: &[&str]| -> Vec<String> {
        let mut dirs: Vec<String> = kept
            .iter()
            .filter(|p| names.contains(&file_name(p)))
            .map(|p| parent(p).to_string())
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
    found
        .into_iter()
        .map(|(dir, ecosystem)| Found {
            harness: has_harness(ecosystem, &dir, &kept, read),
            dir,
            ecosystem,
        })
        .collect()
}

/// Whether the manifests under `dir` name the harness `ecosystem`'s default
/// command runs.
fn has_harness(
    ecosystem: Ecosystem,
    dir: &str,
    files: &[&str],
    read: &dyn Fn(&str) -> String,
) -> bool {
    let under: Vec<&str> = files
        .iter()
        .copied()
        .filter(|p| contains(dir, parent(p)))
        .collect();
    let (names, needles): (&[&str], &[&str]) = match ecosystem {
        Ecosystem::Gradle => (
            &[
                "build.gradle",
                "build.gradle.kts",
                "settings.gradle",
                "settings.gradle.kts",
            ],
            &["kotlinx.benchmark", "jmh"],
        ),
        Ecosystem::Maven => (&["pom.xml"], &["jmh"]),
        Ecosystem::Python => (
            &["pyproject.toml", "setup.py"],
            &["pytest-benchmark", "pytest_benchmark", "pyperf"],
        ),
        Ecosystem::Node => return vitest_bench(&under, read),
        Ecosystem::Rust => return false,
    };
    under
        .iter()
        .filter(|p| names.contains(&file_name(p)))
        .any(|p| {
            let text = read(p).to_lowercase();
            needles.iter().any(|n| text.contains(n))
        })
}

/// Vitest is named by a `package.json`, and either a script runs `bench` or
/// a `*.bench.*` file is tracked.
fn vitest_bench(under: &[&str], read: &dyn Fn(&str) -> String) -> bool {
    let manifests: Vec<String> = under
        .iter()
        .filter(|p| file_name(p) == "package.json")
        .map(|p| read(p))
        .collect();
    let bench_script = |text: &String| {
        serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|v| v["scripts"].as_object().cloned())
            .is_some_and(|scripts| {
                scripts.contains_key("bench")
                    || scripts
                        .values()
                        .any(|cmd| cmd.as_str().is_some_and(|c| c.contains("vitest bench")))
            })
    };
    manifests.iter().any(|m| m.contains("vitest"))
        && (manifests.iter().any(bench_script)
            || under.iter().any(|p| file_name(p).contains(".bench.")))
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or(".", |(dir, _)| dir)
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
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
    use super::*;
    use Ecosystem::*;

    const EVIDENCE: &str = r#"{"scripts":{"bench":"vitest bench"},"devDependencies":{"vitest":"1"},"x":"pytest-benchmark jmh"}"#;

    fn found(paths: &[&str]) -> Vec<(String, Ecosystem)> {
        from_paths(paths, &|_| EVIDENCE.into())
            .into_iter()
            .map(|f| (f.dir, f.ecosystem))
            .collect()
    }

    fn harness(paths: &[&str], files: &[(&str, &str)]) -> Vec<bool> {
        let read = |path: &str| {
            let hit = files.iter().find(|(p, _)| *p == path);
            hit.map(|(_, text)| text.to_string()).unwrap_or_default()
        };
        from_paths(paths, &read)
            .into_iter()
            .map(|f| f.harness)
            .collect()
    }

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
    fn a_python_project_needs_a_benchmark_plugin_named() {
        let paths = ["a/pyproject.toml", "b/pyproject.toml", "c/setup.py"];
        let files = [
            ("a/pyproject.toml", "[tool]\nname = 'a'\n"),
            ("b/pyproject.toml", "dev = [\"pytest-benchmark>=4\"]"),
            ("c/setup.py", "install_requires=['pyperf']"),
        ];
        assert_eq!(harness(&paths, &files), [false, true, true]);
    }

    #[test]
    fn a_node_project_needs_vitest_and_a_bench_script_or_file() {
        let tooling = r#"{"devDependencies":{"prettier":"3"}}"#;
        let script = r#"{"scripts":{"bench":"vitest bench"},"devDependencies":{"vitest":"1"}}"#;
        let no_bench = r#"{"scripts":{"test":"vitest"},"devDependencies":{"vitest":"1"}}"#;
        let paths = [
            "a/package.json",
            "b/package.json",
            "c/package.json",
            "c/src/sort.bench.ts",
            "d/package.json",
            "d/src/sort.bench.ts",
        ];
        let files = [
            ("a/package.json", tooling),
            ("b/package.json", script),
            ("c/package.json", no_bench),
            ("d/package.json", tooling),
        ];
        assert_eq!(harness(&paths, &files), [false, true, true, false]);
    }

    #[test]
    fn gradle_and_maven_need_jmh_or_kotlinx_benchmark_named() {
        let paths = [
            "g1/settings.gradle.kts",
            "g1/app/build.gradle.kts",
            "g2/build.gradle",
            "g3/build.gradle.kts",
            "m1/pom.xml",
            "m2/pom.xml",
        ];
        let files = [
            (
                "g1/app/build.gradle.kts",
                "id(\"me.champeau.jmh\") version \"0.7\"",
            ),
            ("g2/build.gradle", "plugins { id 'java' }"),
            (
                "g3/build.gradle.kts",
                "id(\"org.jetbrains.kotlinx.benchmark\")",
            ),
            ("m1/pom.xml", "<artifactId>jmh-core</artifactId>"),
            ("m2/pom.xml", "<project/>"),
        ];
        assert_eq!(harness(&paths, &files), [true, false, true, true, false]);
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
        let dirs: Vec<_> = projects(&root).into_iter().map(|f| f.dir).collect();
        assert_eq!(dirs, ["client"]);
    }
}
