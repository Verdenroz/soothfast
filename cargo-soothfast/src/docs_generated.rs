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

/// Generated files present under `pkg_dir`, as package-relative paths.
fn owned_files(pkg_dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for (dir, _) in gentests::OWNED {
        let Ok(entries) = std::fs::read_dir(pkg_dir.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let rel = format!("{dir}/{}", entry.file_name().to_string_lossy());
            let generated = gentests::in_owned_place(&rel)
                && std::fs::read_to_string(entry.path())
                    .is_ok_and(|t| t.starts_with(gentests::GENERATED_HEADER));
            if generated {
                found.push(rel);
            }
        }
    }
    found.sort();
    found
}

/// What differs from `expected`, one line per file, each naming the command
/// that fixes it. Empty when disk matches.
pub fn check(pkg_dir: &Path, pkg: &str, expected: &[Generated]) -> Vec<String> {
    let fix = format!("run `cargo soothfast docs gen-tests -p {pkg}`");
    let mut problems = Vec::new();
    for g in expected {
        match std::fs::read_to_string(pkg_dir.join(&g.path)) {
            Ok(actual) if actual == g.content => {}
            Ok(_) => problems.push(format!("{}: generated file is stale — {fix}", g.path)),
            Err(_) => problems.push(format!("{}: generated file is missing — {fix}", g.path)),
        }
    }
    let wanted: BTreeSet<&str> = expected.iter().map(|g| g.path.as_str()).collect();
    for path in owned_files(pkg_dir) {
        if wanted.contains(path.as_str()) {
            continue;
        }
        let why = match legacy_replacement(&path) {
            Some(new) => format!("replaced by {new}/"),
            None => "no longer generated".into(),
        };
        problems.push(format!("{path}: {why} — {fix}"));
    }
    problems
}

/// Where a file of an earlier release's layout now lives, if it is one.
fn legacy_replacement(path: &str) -> Option<&'static str> {
    let (dir, name) = path.rsplit_once('/')?;
    (dir == "tests" && name.starts_with("soothfast_doc_")).then_some(gentests::DOC_TESTS_DIR)
}

/// Write `expected` and remove every other generated file. Returns the paths
/// written and removed.
pub fn write(pkg_dir: &Path, expected: &[Generated]) -> io::Result<(Vec<String>, Vec<String>)> {
    let mut written = Vec::new();
    for g in expected {
        let path = pkg_dir.join(&g.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &g.content)?;
        written.push(g.path.clone());
    }
    let wanted: BTreeSet<&str> = expected.iter().map(|g| g.path.as_str()).collect();
    let mut removed = Vec::new();
    for path in owned_files(pkg_dir) {
        if !wanted.contains(path.as_str()) {
            std::fs::remove_file(pkg_dir.join(&path))?;
            removed.push(path);
        }
    }
    for (dir, _) in gentests::OWNED {
        let dir = pkg_dir.join(dir);
        if dir != pkg_dir.join("tests") && dir.read_dir().is_ok_and(|mut d| d.next().is_none()) {
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
        assert!(check(&pkg.0, "p", &expected()).is_empty());
    }

    #[test]
    fn a_leftover_per_page_file_fails_and_names_gen_tests() {
        let pkg = Pkg::new("legacy");
        write(&pkg.0, &expected()).unwrap();
        let old = generated("tests/soothfast_doc_readme.rs", "");
        pkg.put(&old.path, &old.content);
        let problems = check(&pkg.0, "p", &expected());
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
        let problems = check(&pkg.0, "p", &expected());
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
        assert!(check(&pkg.0, "p", &expected()).is_empty());
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
