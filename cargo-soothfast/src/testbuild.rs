//! Building the selected packages' test executables.

use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::testsel::Package;

/// One test executable cargo built.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TestBin {
    pub package: String,
    pub target: String,
    /// The target kind: `lib`, `bin`, `test`, `bench`, `example`.
    pub kind: String,
    pub exe: PathBuf,
}

impl TestBin {
    /// `package::target (kind)`, how the summary names a binary.
    pub fn label(&self) -> String {
        format!("{} {} ({})", self.package, self.target, self.kind)
    }
}

/// `cargo test --no-run` for `names`, returning cargo's JSON on stdout.
/// Compiler diagnostics are echoed to stderr as they would read normally.
pub fn build(names: &[String], cargo_args: &[String]) -> io::Result<Vec<u8>> {
    let mut cmd = Command::new("cargo");
    cmd.args(["test", "--no-run", "--message-format=json"]);
    for n in names {
        cmd.args(["-p", n]);
    }
    cmd.args(cargo_args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());
    let out = cmd.output()?;
    for rendered in rendered_diagnostics(&out.stdout) {
        eprint!("{rendered}");
    }
    if !out.status.success() {
        return Err(io::Error::other("cargo test --no-run failed"));
    }
    Ok(out.stdout)
}

fn messages(stdout: &[u8]) -> impl Iterator<Item = Value> + '_ {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect::<Vec<_>>()
        .into_iter()
}

fn rendered_diagnostics(stdout: &[u8]) -> Vec<String> {
    messages(stdout)
        .filter(|m| m["reason"] == "compiler-message")
        .filter_map(|m| m["message"]["rendered"].as_str().map(String::from))
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

/// Every workspace executable named in cargo's JSON, tagged with the
/// package that owns it.
pub fn collect(stdout: &[u8], pkgs: &[Package]) -> Built {
    let mut tests = BTreeSet::new();
    let mut siblings = BTreeSet::new();
    for m in messages(stdout).filter(|m| m["reason"] == "compiler-artifact") {
        let Some(bin) = executable(&m, pkgs) else {
            continue;
        };
        if m["profile"]["test"] == true {
            tests.insert(bin);
        } else if matches!(bin.kind.as_str(), "bin" | "example") {
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
    Some(TestBin {
        package: package.name.clone(),
        target: m["target"]["name"].as_str()?.to_string(),
        kind: m["target"]["kind"][0].as_str()?.to_string(),
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
            env: Vec::new(),
        }
    }

    fn artifact(manifest: &str, target: &str, kind: &str, test: bool, exe: Option<&str>) -> String {
        json!({ "reason": "compiler-artifact", "manifest_path": manifest,
                "target": { "name": target, "kind": [kind] },
                "profile": { "test": test }, "executable": exe })
        .to_string()
    }

    #[test]
    fn test_executables_are_collected_with_their_package_and_kind() {
        let out = [
            artifact("/ws/a/Cargo.toml", "a", "lib", true, Some("/t/a-1")),
            artifact("/ws/a/Cargo.toml", "it", "test", true, Some("/t/it-2")),
            artifact("/ws/b/Cargo.toml", "b", "bin", true, Some("/t/b-3")),
        ]
        .join("\n");
        let bins = collect(out.as_bytes(), &[pkg("a"), pkg("b")]).tests;
        let labels: Vec<String> = bins.iter().map(TestBin::label).collect();
        assert_eq!(labels, ["a a (lib)", "a it (test)", "b b (bin)"]);
        assert_eq!(bins[1].exe, PathBuf::from("/t/it-2"));
    }

    #[test]
    fn non_test_and_non_executable_artifacts_are_skipped() {
        let out = [
            artifact("/ws/a/Cargo.toml", "a", "lib", false, Some("/t/a-lib")),
            artifact("/ws/a/Cargo.toml", "a", "lib", true, None),
            artifact(
                "/ws/a/Cargo.toml",
                "build-script",
                "custom-build",
                false,
                None,
            ),
        ]
        .join("\n");
        assert_eq!(collect(out.as_bytes(), &[pkg("a")]), Built::default());
    }

    #[test]
    fn plain_bins_and_examples_are_siblings_not_tests() {
        let out = [
            artifact("/ws/a/Cargo.toml", "a", "bin", false, Some("/t/a")),
            artifact(
                "/ws/a/Cargo.toml",
                "demo",
                "example",
                false,
                Some("/t/demo"),
            ),
            artifact("/ws/a/Cargo.toml", "a", "lib", false, Some("/t/liba.rlib")),
        ]
        .join("\n");
        let built = collect(out.as_bytes(), &[pkg("a")]);
        assert!(built.tests.is_empty());
        let labels: Vec<String> = built.siblings.iter().map(TestBin::label).collect();
        assert_eq!(labels, ["a a (bin)", "a demo (example)"]);
    }

    #[test]
    fn a_repeated_artifact_is_one_binary() {
        let line = artifact("/ws/a/Cargo.toml", "a", "lib", true, Some("/t/a-1"));
        let out = format!("{line}\n{line}\nnot json\n");
        assert_eq!(collect(out.as_bytes(), &[pkg("a")]).tests.len(), 1);
    }

    #[test]
    fn a_dependency_outside_the_workspace_is_not_collected() {
        let out = artifact("/reg/serde/Cargo.toml", "serde", "lib", true, Some("/t/s"));
        assert_eq!(collect(out.as_bytes(), &[pkg("a")]), Built::default());
    }
}
