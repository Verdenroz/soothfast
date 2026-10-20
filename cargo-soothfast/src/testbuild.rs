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

/// Every test executable named in cargo's JSON, tagged with the workspace
/// package that owns it.
pub fn collect(stdout: &[u8], pkgs: &[Package]) -> Vec<TestBin> {
    let bins: BTreeSet<TestBin> = messages(stdout)
        .filter(|m| m["reason"] == "compiler-artifact" && m["profile"]["test"] == true)
        .filter_map(|m| {
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
        })
        .collect();
    bins.into_iter().collect()
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
        let bins = collect(out.as_bytes(), &[pkg("a"), pkg("b")]);
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
        assert!(collect(out.as_bytes(), &[pkg("a")]).is_empty());
    }

    #[test]
    fn a_repeated_artifact_is_one_binary() {
        let line = artifact("/ws/a/Cargo.toml", "a", "lib", true, Some("/t/a-1"));
        let out = format!("{line}\n{line}\nnot json\n");
        assert_eq!(collect(out.as_bytes(), &[pkg("a")]).len(), 1);
    }

    #[test]
    fn a_dependency_outside_the_workspace_is_not_collected() {
        let out = artifact("/reg/serde/Cargo.toml", "serde", "lib", true, Some("/t/s"));
        assert!(collect(out.as_bytes(), &[pkg("a")]).is_empty());
    }
}
