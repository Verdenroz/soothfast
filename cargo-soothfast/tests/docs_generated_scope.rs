//! A PATHS-scoped `docs gen-tests`, `docs check` or `docs capture` acts on
//! the package's whole generated set: pages in scope plus every page its
//! generated files already record.

use std::path::PathBuf;
use std::process::Command;

const PAGE_A: &str = "# A\n\n```rust\nassert_eq!(1 + 1, 2);\n```\n\n```rust capture-output\nprintln!(\"from a\");\n```\n";
const PAGE_B: &str = "# B\n\n```rust\nassert!(true);\n```\n\n```rust capture-output feature=extra\nprintln!(\"from b\");\n```\n";

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("soothfast-docs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let fx = Fixture { root };
        fx.put(
            "Cargo.toml",
            "[workspace]\nmembers = [\"p\"]\nresolver = \"2\"\n",
        );
        fx.put(
            "p/Cargo.toml",
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nextra = []\n",
        );
        fx.put("p/src/lib.rs", "");
        fx.put("docs/a.md", PAGE_A);
        fx.put("docs/b.md", PAGE_B);
        fx
    }

    fn put(&self, rel: &str, text: &str) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn has(&self, rel: &str) -> bool {
        self.root.join(rel).exists()
    }

    /// `cargo soothfast docs ARGS`: whether it passed, and what it printed.
    fn docs(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_cargo-soothfast"))
            .args(["soothfast", "docs"])
            .args(args)
            .current_dir(&self.root)
            .env("CARGO_TARGET_DIR", self.root.join("target"))
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }

    fn generated(&self) -> Vec<String> {
        let mut found = Vec::new();
        for dir in ["p/tests/soothfast_docs", "p/examples/soothfast_capture"] {
            for entry in std::fs::read_dir(self.root.join(dir)).into_iter().flatten() {
                found.push(format!(
                    "{dir}/{}",
                    entry.unwrap().file_name().to_string_lossy()
                ));
            }
        }
        found.sort();
        found
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn both(fx: &Fixture) -> Vec<String> {
    let (ok, out) = fx.docs(&["gen-tests", "-p", "p", "docs/a.md", "docs/b.md"]);
    assert!(ok, "{out}");
    fx.generated()
}

#[test]
fn a_scoped_run_keeps_the_pages_the_package_already_generates() {
    let fx = Fixture::new("scoped");
    let all = both(&fx);
    assert_eq!(all.len(), 6, "{all:?}");

    let (ok, out) = fx.docs(&["gen-tests", "-p", "p", "docs/a.md"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("also kept: 1 page(s) already generated for p"),
        "{out}"
    );
    assert!(out.contains("0 written, 0 removed, 6 unchanged"), "{out}");
    assert_eq!(fx.generated(), all);
    assert!(
        fx.read("p/examples/soothfast_capture/main.rs")
            .contains("\"docs_b_1\" => run(block_docs_b_1::main),")
    );

    let (ok, out) = fx.docs(&["check", "-p", "p", "docs/a.md"]);
    assert!(ok, "{out}");
}

#[test]
fn a_recorded_page_that_is_gone_or_has_no_blocks_left_is_dropped() {
    let fx = Fixture::new("dropped");
    both(&fx);

    fx.put("docs/b.md", "# B\n\n```rust ignore\nnope();\n```\n");
    let (ok, out) = fx.docs(&["gen-tests", "-p", "p", "docs/a.md"]);
    assert!(ok, "{out}");
    assert!(!fx.has("p/tests/soothfast_docs/doc_docs_b.rs"), "{out}");
    assert!(
        !fx.has("p/examples/soothfast_capture/block_docs_b_1.rs"),
        "{out}"
    );
    assert!(
        !fx.read("p/examples/soothfast_capture/main.rs")
            .contains("docs_b_1")
    );

    fx.put("docs/b.md", PAGE_B);
    both(&fx);
    std::fs::remove_file(fx.root.join("docs/a.md")).unwrap();
    let (ok, out) = fx.docs(&["gen-tests", "-p", "p", "docs/b.md"]);
    assert!(ok, "{out}");
    assert!(!fx.has("p/tests/soothfast_docs/doc_docs_a.rs"), "{out}");
    assert!(
        !fx.has("p/examples/soothfast_capture/block_docs_a_1.rs"),
        "{out}"
    );
}

#[test]
fn a_scoped_capture_runs_only_its_pages_and_a_disabled_block_keeps_its_output() {
    let fx = Fixture::new("capture");
    both(&fx);
    let (ok, out) = fx.docs(&[
        "capture",
        "-p",
        "p",
        "--features",
        "extra",
        "docs/a.md",
        "docs/b.md",
    ]);
    assert!(ok, "{out}");
    assert!(out.contains("2 checked, 2 stale, 0 skipped"), "{out}");
    let page_b = fx.read("docs/b.md");
    assert!(page_b.contains("from b"), "{page_b}");

    let (ok, out) = fx.docs(&["capture", "-p", "p", "--check", "docs/a.md"]);
    assert!(ok, "{out}");
    assert!(out.contains("1 checked, 0 stale, 0 skipped"), "{out}");
    assert!(!out.contains("docs/b.md"), "{out}");

    let (ok, out) = fx.docs(&["capture", "-p", "p", "docs/b.md"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("skip  docs/b.md: block docs_b_1 (features not enabled)"),
        "{out}"
    );
    assert!(out.contains("0 checked, 0 stale, 1 skipped"), "{out}");
    assert_eq!(fx.read("docs/b.md"), page_b);
}

#[test]
fn a_capture_example_older_than_its_page_fails_naming_gen_tests() {
    let fx = Fixture::new("unknown");
    both(&fx);
    fx.put(
        "docs/a.md",
        &format!("{PAGE_A}\n```rust capture-output\nprintln!(\"new\");\n```\n"),
    );
    let (ok, out) = fx.docs(&["capture", "-p", "p", "docs/a.md"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("block docs_a_2 is not in the capture example — run `cargo soothfast docs gen-tests -p p`"),
        "{out}"
    );
}

#[test]
fn a_block_that_does_not_compile_is_one_failure_naming_its_page() {
    let fx = Fixture::new("uncompiled");
    fx.put(
        "p/src/lib.rs",
        "#[cfg(feature = \"extra\")]\npub fn gated() {}\n",
    );
    fx.put(
        "docs/c.md",
        "# C\n\n```rust capture-output\np::gated();\n```\n",
    );
    let (ok, out) = fx.docs(&["gen-tests", "-p", "p", "docs/a.md", "docs/c.md"]);
    assert!(ok, "{out}");
    let (ok, out) = fx.docs(&["capture", "-p", "p", "docs/a.md", "docs/c.md"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains(
            "FAIL  examples/soothfast_capture: the shared capture example does not compile"
        ),
        "{out}"
    );
    assert!(
        out.contains("error in docs/c.md line 3 (block_docs_c_1.rs)"),
        "{out}"
    );
    assert!(!out.contains("exited with"), "{out}");
    assert!(!out.contains("docs/a.md line"), "{out}");
}
