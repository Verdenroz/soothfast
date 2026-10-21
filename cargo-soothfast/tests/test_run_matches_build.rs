//! `cargo soothfast test` runs every binary in the build's own invocation.
//!
//! The fixture is shaped so a per-package run would differ from the build:
//! `b` turns on `a/extra`, so `-p a` alone resolves `a` without it, and `d`
//! does not depend on `a`, so `-p d --features a/extra` is an error.

use std::path::PathBuf;
use std::process::Command;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("soothfast-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let files: &[(&str, &str)] = &[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"a\", \"b\", \"d\"]\nresolver = \"2\"\n",
            ),
            (".gitignore", "target\n.soothfast\n"),
            (
                "a/Cargo.toml",
                "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nextra = []\n",
            ),
            (
                "a/src/lib.rs",
                "/// ```\n/// assert!(a::extra());\n/// ```\npub fn extra() -> bool {\n    cfg!(feature = \"extra\")\n}\n\n#[cfg(test)]\nmod t {\n    #[test]\n    fn extra_is_on() {\n        assert!(super::extra());\n    }\n}\n",
            ),
            (
                "a/src/bin/tool.rs",
                "fn main() {\n    print!(\"{}\", a::extra());\n}\n",
            ),
            ("a/examples/demo.rs", "fn main() {}\n"),
            (
                "a/tests/it.rs",
                "#[test]\nfn the_bin_sees_extra() {\n    let out = std::process::Command::new(env!(\"CARGO_BIN_EXE_tool\")).output().unwrap();\n    assert_eq!(out.stdout, b\"true\");\n}\n",
            ),
            (
                "b/Cargo.toml",
                "[package]\nname = \"b\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\na = { path = \"../a\", features = [\"extra\"] }\n",
            ),
            (
                "b/src/lib.rs",
                "#[cfg(test)]\nmod t {\n    #[test]\n    fn uses_a() {\n        assert!(a::extra());\n    }\n}\n",
            ),
            (
                "d/Cargo.toml",
                "[package]\nname = \"d\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            (
                "d/src/lib.rs",
                "#[cfg(test)]\nmod t {\n    #[test]\n    fn alone() {}\n}\n",
            ),
        ];
        for (path, text) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let git = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(git.success());
        Fixture { root }
    }

    /// `cargo soothfast test --all --features a/extra ARGS`: whether it
    /// passed, and everything it printed.
    fn test(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_cargo-soothfast"))
            .args(["soothfast", "test", "--all", "--features", "a/extra"])
            .args(args)
            .current_dir(&self.root)
            .env("CARGO_TARGET_DIR", self.root.join("target"))
            .env_remove("RUSTDOC")
            .env_remove("SOOTHFAST_TEST_PLAN")
            .env_remove("CARGO_BUILD_TARGET")
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }

    fn sibling(&self) -> Vec<u8> {
        std::fs::read(
            self.root
                .join("target/debug")
                .join(format!("tool{}", std::env::consts::EXE_SUFFIX)),
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn summary(text: &str) -> &str {
    text.lines()
        .rev()
        .find(|l| l.contains("packages selected"))
        .unwrap_or_default()
}

fn compiles_after_first_run(text: &str) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    let first_run = lines
        .iter()
        .position(|l| l.trim_start().starts_with("Running "))
        .unwrap_or(lines.len());
    lines[first_run..]
        .iter()
        .copied()
        .filter(|l| l.trim_start().starts_with("Compiling "))
        .collect()
}

#[test]
fn every_binary_runs_in_the_builds_invocation_and_its_pass_is_reused() {
    let fx = Fixture::new("reused");

    let (ok, cold) = fx.test(&[]);
    assert!(ok, "{cold}");
    assert_eq!(
        compiles_after_first_run(&cold),
        Vec::<&str>::new(),
        "{cold}"
    );
    assert!(!cold.contains("changed its inputs"), "{cold}");
    assert!(
        summary(&cold).contains("0 skipped (cached), doc tests in 3 packages"),
        "{cold}"
    );
    let sibling = fx.sibling();

    let (ok, warm) = fx.test(&[]);
    assert!(ok, "{warm}");
    assert!(summary(&warm).contains(" 0 binaries run"), "{warm}");
    assert!(
        warm.contains("test: skip d d (lib) (cached pass)"),
        "{warm}"
    );
    assert_eq!(
        fx.sibling(),
        sibling,
        "the bin an integration test spawns moved between runs"
    );

    let (ok, no_doc) = fx.test(&["--no-doc"]);
    assert!(ok, "{no_doc}");
    assert!(
        summary(&no_doc).contains(" 0 binaries run"),
        "--no-doc keyed differently: {no_doc}"
    );
    assert!(
        no_doc.contains("test: skip doc tests for a (--no-doc)"),
        "{no_doc}"
    );
    assert!(
        summary(&no_doc).contains("doc tests in 0 packages"),
        "{no_doc}"
    );
}

/// soothfast testing its own workspace relinks the binary it runs from
/// during the build, so the runner cargo calls must not be that file.
#[cfg(target_os = "linux")]
#[test]
fn the_runner_survives_its_own_binary_being_replaced() {
    let fx = Fixture::new("replaced");
    let cli = fx.root.join("cli-copy");
    std::fs::copy(env!("CARGO_BIN_EXE_cargo-soothfast"), &cli).unwrap();
    let child = Command::new(&cli)
        .args(["soothfast", "test", "--all", "--features", "a/extra"])
        .current_dir(&fx.root)
        .env("CARGO_TARGET_DIR", fx.root.join("target"))
        .env_remove("RUSTDOC")
        .env_remove("SOOTHFAST_TEST_PLAN")
        .env_remove("CARGO_BUILD_TARGET")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::fs::remove_file(&cli).unwrap();
    let out = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(!text.contains("never executed"), "{text}");
}

#[test]
fn a_failing_binary_is_named_and_the_rest_still_pass() {
    let fx = Fixture::new("failing");
    let it = fx.root.join("a/tests/it.rs");
    let text = std::fs::read_to_string(&it)
        .unwrap()
        .replace("b\"true\"", "b\"false\"");
    std::fs::write(&it, text).unwrap();

    let (ok, out) = fx.test(&[]);
    assert!(!ok, "{out}");
    let failed = out
        .lines()
        .find(|l| l.starts_with("test: FAILED:"))
        .unwrap_or_default();
    assert_eq!(failed, "test: FAILED: a it (test)", "{out}");

    let (_, again) = fx.test(&[]);
    assert!(
        again.contains("test: skip b b (lib) (cached pass)"),
        "{again}"
    );
    assert!(
        !again.contains("test: skip a it (test)"),
        "a failure was stored as a pass: {again}"
    );
}
