//! Keeping a package's generated files in step with what gen-tests would
//! write: compared by `docs check`, written by `docs gen-tests`.
//!
//! Only files in a place gen-tests owns ([`gentests::OWNED`]) that start with
//! [`gentests::GENERATED_HEADER`] are compared or removed, so a hand-written
//! test beside them is never touched.

use std::collections::BTreeSet;
use std::io;
use std::path::Path;

use soothfast_docs::gentests::{self, Generated};

/// Generated files under `pkg_dir` as (package-relative path, content); with
/// `only`, just that target's.
fn owned_files(pkg_dir: &Path, only: Option<&str>) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for (dir, _, _) in gentests::OWNED {
        let Ok(entries) = std::fs::read_dir(pkg_dir.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let rel = format!("{dir}/{}", entry.file_name().to_string_lossy());
            let Some((target, _)) = gentests::owning_target(&rel) else {
                continue;
            };
            if only.is_some_and(|t| t != target) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(entry.path())
                && text.starts_with(gentests::GENERATED_HEADER)
            {
                found.push((rel, text));
            }
        }
    }
    found.sort();
    found
}

/// Pages the package's generated files were generated from, workspace-relative.
pub fn recorded_pages(pkg_dir: &Path) -> BTreeSet<String> {
    owned_files(pkg_dir, None)
        .iter()
        .filter_map(|(_, text)| gentests::recorded_source(text).map(|(page, _)| page))
        .map(page_key)
        .collect()
}

/// A workspace-relative page path in the one spelling records and PATHS share.
pub fn page_key(page: &str) -> String {
    page.replace('\\', "/").trim_start_matches("./").to_string()
}

/// What differs from `expected`, one line per file naming the command that
/// fixes it; with `only`, just that target's files. Empty when disk matches.
pub fn check(pkg_dir: &Path, pkg: &str, expected: &[Generated], only: Option<&str>) -> Vec<String> {
    let fix = format!("run `cargo soothfast docs gen-tests -p {pkg}`");
    let mut problems = Vec::new();
    for g in expected {
        match std::fs::read_to_string(pkg_dir.join(&g.path)) {
            Ok(actual) if actual == g.content => {}
            Ok(_) => problems.push(format!("{}: generated file is stale — {fix}", g.path)),
            Err(_) => problems.push(format!("{}: generated file is missing — {fix}", g.path)),
        }
    }
    for path in unexpected(pkg_dir, expected, only) {
        let why = match gentests::owning_target(&path) {
            Some((target, true)) => format!("replaced by {target}/"),
            _ => "no longer generated".into(),
        };
        problems.push(format!("{path}: {why} — {fix}"));
    }
    problems
}

/// Generated files on disk that `expected` no longer produces.
fn unexpected(pkg_dir: &Path, expected: &[Generated], only: Option<&str>) -> Vec<String> {
    let wanted: BTreeSet<&str> = expected.iter().map(|g| g.path.as_str()).collect();
    owned_files(pkg_dir, only)
        .into_iter()
        .map(|(path, _)| path)
        .filter(|path| !wanted.contains(path.as_str()))
        .collect()
}

/// Write the files of `expected` whose bytes differ on disk, and remove every
/// other generated file. Returns the paths written and removed.
pub fn write(pkg_dir: &Path, expected: &[Generated]) -> io::Result<(Vec<String>, Vec<String>)> {
    let mut written = Vec::new();
    for g in expected {
        let path = pkg_dir.join(&g.path);
        if std::fs::read_to_string(&path).is_ok_and(|t| t == g.content) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &g.content)?;
        written.push(g.path.clone());
    }
    let removed = unexpected(pkg_dir, expected, None);
    for path in &removed {
        std::fs::remove_file(pkg_dir.join(path))?;
    }
    for (dir, prefix, _) in gentests::OWNED {
        let dir = pkg_dir.join(dir);
        if prefix.is_empty() && dir.read_dir().is_ok_and(|mut d| d.next().is_none()) {
            std::fs::remove_dir(dir)?;
        }
    }
    Ok((written, removed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Pkg(PathBuf);

    impl Pkg {
        fn new(name: &str) -> Pkg {
            let dir =
                std::env::temp_dir().join(format!("soothfast-gen-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Pkg(dir)
        }

        fn put(&self, rel: &str, content: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }

        fn has(&self, rel: &str) -> bool {
            self.0.join(rel).exists()
        }
    }

    impl Drop for Pkg {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn generated(path: &str, body: &str) -> Generated {
        Generated {
            path: path.into(),
            content: format!("{}\n{body}", gentests::GENERATED_HEADER),
        }
    }

    fn expected() -> Vec<Generated> {
        vec![
            generated("tests/soothfast_docs/main.rs", "mod doc_readme;\n"),
            generated("tests/soothfast_docs/doc_readme.rs", "#[test]\nfn t() {}\n"),
        ]
    }

    #[test]
    fn a_matching_package_has_no_problems() {
        let pkg = Pkg::new("match");
        write(&pkg.0, &expected()).unwrap();
        assert!(check(&pkg.0, "p", &expected(), None).is_empty());
    }

    #[test]
    fn a_leftover_per_page_file_fails_and_names_gen_tests() {
        let pkg = Pkg::new("legacy");
        write(&pkg.0, &expected()).unwrap();
        let old = generated("tests/soothfast_doc_readme.rs", "");
        pkg.put(&old.path, &old.content);
        let problems = check(&pkg.0, "p", &expected(), None);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0]
                .starts_with("tests/soothfast_doc_readme.rs: replaced by tests/soothfast_docs/")
        );
        assert!(problems[0].contains("cargo soothfast docs gen-tests -p p"));
    }

    #[test]
    fn a_stale_or_orphaned_page_module_fails() {
        let pkg = Pkg::new("stale");
        write(&pkg.0, &expected()).unwrap();
        pkg.put(
            "tests/soothfast_docs/doc_readme.rs",
            gentests::GENERATED_HEADER,
        );
        let orphan = generated("tests/soothfast_docs/doc_gone.rs", "");
        pkg.put(&orphan.path, &orphan.content);
        let problems = check(&pkg.0, "p", &expected(), None);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].contains("doc_readme.rs: generated file is stale"));
        assert!(problems[1].contains("doc_gone.rs: no longer generated"));
    }

    #[test]
    fn gen_tests_removes_owned_files_and_leaves_hand_written_ones() {
        let pkg = Pkg::new("write");
        let old = generated("tests/soothfast_doc_readme.rs", "");
        pkg.put(&old.path, &old.content);
        pkg.put("tests/soothfast_doc_mine.rs", "// hand-written\n");
        pkg.put("tests/integration.rs", "// hand-written\n");
        let (written, removed) = write(&pkg.0, &expected()).unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(removed, ["tests/soothfast_doc_readme.rs"]);
        assert!(pkg.has("tests/soothfast_doc_mine.rs"));
        assert!(pkg.has("tests/integration.rs"));
        assert!(check(&pkg.0, "p", &expected(), None).is_empty());
    }

    #[test]
    fn unchanged_files_are_not_rewritten() {
        let pkg = Pkg::new("unchanged");
        write(&pkg.0, &expected()).unwrap();
        let mut next = expected();
        next[1].content.push_str("// changed\n");
        let (written, removed) = write(&pkg.0, &next).unwrap();
        assert_eq!(written, ["tests/soothfast_docs/doc_readme.rs"]);
        assert!(removed.is_empty());
    }

    #[test]
    fn recorded_pages_come_only_from_generated_files_in_owned_places() {
        let pkg = Pkg::new("recorded");
        let h = gentests::GENERATED_HEADER;
        pkg.put(
            "tests/soothfast_docs/doc_a.rs",
            &format!("{h}\n// source: ./docs/a.md\n"),
        );
        pkg.put(
            "examples/soothfast_capture/block_b_1.rs",
            &format!("{h}\n// source: docs/b.md line 7\n"),
        );
        pkg.put(
            "tests/soothfast_doc_c.rs",
            &format!("{h}\n// source: docs/c.md\n"),
        );
        pkg.put("tests/soothfast_docs/doc_d.rs", "// source: docs/d.md\n");
        pkg.put("tests/helper.rs", &format!("{h}\n// source: docs/e.md\n"));
        let pages: Vec<String> = recorded_pages(&pkg.0).into_iter().collect();
        assert_eq!(pages, ["docs/a.md", "docs/b.md", "docs/c.md"]);
    }

    #[test]
    fn a_leftover_per_block_capture_example_fails_and_names_the_new_place() {
        let pkg = Pkg::new("legacy-capture");
        let old = generated("examples/soothfast_capture_readme_1.rs", "");
        pkg.put(&old.path, &old.content);
        let problems = check(&pkg.0, "p", &[], None);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("replaced by examples/soothfast_capture/"));
    }

    #[test]
    fn nothing_testable_removes_the_doc_test_target() {
        let pkg = Pkg::new("empty");
        write(&pkg.0, &expected()).unwrap();
        let (_, removed) = write(&pkg.0, &[]).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(!pkg.has("tests/soothfast_docs"));
    }
}
