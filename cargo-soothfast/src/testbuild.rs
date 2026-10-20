//! Building the selected packages' test executables.

use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::testsel::Package;

/// Target kinds that make up a package's library.
const LIB_KINDS: &[&str] = &["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"];

/// One test executable cargo built.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TestBin {
    pub package: String,
    pub target: String,
    /// The target's kinds: `lib`, `bin`, `test`, `bench`, `example`, and
    /// crate types such as `cdylib` alongside `rlib`.
    pub kinds: Vec<String>,
    pub exe: PathBuf,
}

impl TestBin {
    /// `package target (kind)`, how the summary names a binary.
    pub fn label(&self) -> String {
        format!(
            "{} {} ({})",
            self.package,
            self.target,
            self.kinds.join(",")
        )
    }

    fn has_kind(&self, wanted: &[&str]) -> bool {
        self.kinds.iter().any(|k| wanted.contains(&k.as_str()))
    }

    /// Whether the package's doc tests belong to this target.
    pub fn has_doc_tests(&self) -> bool {
        self.has_kind(&["lib", "rlib", "proc-macro"])
    }

    /// The cargo flags that select exactly this target.
    pub fn target_flags(&self) -> Vec<String> {
        if self.has_kind(LIB_KINDS) {
            return vec!["--lib".into()];
        }
        let kind = self.kinds.first().map_or("bin", String::as_str);
        vec![format!("--{kind}"), self.target.clone()]
    }
}

/// `cargo test --no-run` for `names`, returning cargo's JSON messages.
/// Compiler diagnostics are echoed to stderr as they would read normally.
pub fn build(names: &[String], cargo_args: &[String]) -> io::Result<Vec<Value>> {
    let mut cmd = Command::new("cargo");
    cmd.args(["test", "--no-run", "--message-format=json"]);
    for n in names {
        cmd.args(["-p", n]);
    }
    cmd.args(cargo_args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());
    let out = cmd.output()?;
    let messages = parse(&out.stdout);
    for m in messages
        .iter()
        .filter(|m| m["reason"] == "compiler-message")
    {
        if let Some(rendered) = m["message"]["rendered"].as_str() {
            eprint!("{rendered}");
        }
    }
    if !out.status.success() {
        return Err(io::Error::other("cargo test --no-run failed"));
    }
    Ok(messages)
}

fn parse(stdout: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

/// What a build produced for the workspace's packages.
#[derive(Debug, Default, PartialEq)]
pub struct Built {
    /// Test executables.
    pub tests: Vec<TestBin>,
    /// Plain `bin` and `example` executables. Integration tests spawn these
    /// without embedding them, so a test's key must cover their code too.
    pub siblings: Vec<TestBin>,
}

/// Every workspace executable named in cargo's messages, tagged with the
/// package that owns it.
pub fn collect(messages: &[Value], pkgs: &[Package]) -> Built {
    let mut tests = BTreeSet::new();
    let mut siblings = BTreeSet::new();
    for m in messages
        .iter()
        .filter(|m| m["reason"] == "compiler-artifact")
    {
        let Some(bin) = executable(m, pkgs) else {
            continue;
        };
        if m["profile"]["test"] == true {
            tests.insert(bin);
        } else if bin.has_kind(&["bin", "example"]) {
            siblings.insert(bin);
        }
    }
    Built {
        tests: tests.into_iter().collect(),
        siblings: siblings.into_iter().collect(),
    }
}

fn executable(m: &Value, pkgs: &[Package]) -> Option<TestBin> {
    let exe = PathBuf::from(m["executable"].as_str()?);
    let manifest = PathBuf::from(m["manifest_path"].as_str()?);
    let package = pkgs
        .iter()
        .find(|p| manifest.parent() == Some(p.manifest_dir.as_path()))?;
    let kinds = m["target"]["kind"]
        .as_array()?
        .iter()
        .filter_map(|k| k.as_str().map(String::from))
        .collect();
    Some(TestBin {
        package: package.name.clone(),
        target: m["target"]["name"].as_str()?.to_string(),
        kinds,
        exe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pkg(name: &str) -> Package {
        Package {
            name: name.into(),
            dir: name.into(),
            manifest_dir: PathBuf::from("/ws").join(name),
            deps: Vec::new(),
        }
    }

    fn artifact(
        manifest: &str,
        target: &str,
        kinds: &[&str],
        test: bool,
        exe: Option<&str>,
    ) -> String {
        json!({ "reason": "compiler-artifact", "manifest_path": manifest,
                "target": { "name": target, "kind": kinds },
                "profile": { "test": test }, "executable": exe })
        .to_string()
    }

    fn collected(lines: &[String], pkgs: &[Package]) -> Built {
        collect(&parse(lines.join("\n").as_bytes()), pkgs)
    }

    #[test]
    fn test_executables_are_collected_with_their_package_and_kind() {
        let out = [
            artifact("/ws/a/Cargo.toml", "a", &["lib"], true, Some("/t/a-1")),
            artifact("/ws/a/Cargo.toml", "it", &["test"], true, Some("/t/it-2")),
            artifact("/ws/b/Cargo.toml", "b", &["bin"], true, Some("/t/b-3")),
        ];
        let bins = collected(&out, &[pkg("a"), pkg("b")]).tests;
        let labels: Vec<String> = bins.iter().map(TestBin::label).collect();
        assert_eq!(labels, ["a a (lib)", "a it (test)", "b b (bin)"]);
        assert_eq!(bins[1].exe, PathBuf::from("/t/it-2"));
    }

    #[test]
    fn non_test_and_non_executable_artifacts_are_skipped() {
        let out = [
            artifact("/ws/a/Cargo.toml", "a", &["lib"], false, Some("/t/a-lib")),
            artifact("/ws/a/Cargo.toml", "a", &["lib"], true, None),
            artifact(
                "/ws/a/Cargo.toml",
                "build-script",
                &["custom-build"],
                false,
                None,
            ),
        ];
        assert_eq!(collected(&out, &[pkg("a")]), Built::default());
    }

    #[test]
    fn plain_bins_and_examples_are_siblings_not_tests() {
        let out = [
            artifact("/ws/a/Cargo.toml", "a", &["bin"], false, Some("/t/a")),
            artifact(
                "/ws/a/Cargo.toml",
                "demo",
                &["example"],
                false,
                Some("/t/demo"),
            ),
            artifact(
                "/ws/a/Cargo.toml",
                "a",
                &["lib"],
                false,
                Some("/t/liba.rlib"),
            ),
        ];
        let built = collected(&out, &[pkg("a")]);
        assert!(built.tests.is_empty());
        let labels: Vec<String> = built.siblings.iter().map(TestBin::label).collect();
        assert_eq!(labels, ["a a (bin)", "a demo (example)"]);
    }

    #[test]
    fn a_repeated_artifact_is_one_binary() {
        let line = artifact("/ws/a/Cargo.toml", "a", &["lib"], true, Some("/t/a-1"));
        let out = [line.clone(), line, "not json".to_string()];
        assert_eq!(collected(&out, &[pkg("a")]).tests.len(), 1);
    }

    #[test]
    fn a_dependency_outside_the_workspace_is_not_collected() {
        let out = [artifact(
            "/reg/serde/Cargo.toml",
            "serde",
            &["lib"],
            true,
            Some("/t/s"),
        )];
        assert_eq!(collected(&out, &[pkg("a")]), Built::default());
    }

    #[test]
    fn a_dual_crate_type_lib_keeps_its_doc_tests_and_selects_with_lib() {
        let out = [artifact(
            "/ws/a/Cargo.toml",
            "a",
            &["cdylib", "rlib"],
            true,
            Some("/t/a"),
        )];
        let bin = collected(&out, &[pkg("a")]).tests.remove(0);
        assert!(bin.has_doc_tests());
        assert_eq!(bin.target_flags(), ["--lib"]);
    }

    #[test]
    fn named_targets_select_by_kind_and_name() {
        let kinds = |k: &str, name: &str| TestBin {
            package: "p".into(),
            target: name.into(),
            kinds: vec![k.into()],
            exe: PathBuf::new(),
        };
        assert_eq!(kinds("test", "golden").target_flags(), ["--test", "golden"]);
        assert_eq!(kinds("bin", "cli").target_flags(), ["--bin", "cli"]);
        assert_eq!(kinds("bench", "b").target_flags(), ["--bench", "b"]);
        assert!(!kinds("test", "golden").has_doc_tests());
    }
}
