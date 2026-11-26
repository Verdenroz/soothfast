//! The instrumented build: flags, target directory and the cargo commands
//! that build and run a package's tests and benches.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::invoke;

const FLAG: &str = "-Cinstrument-coverage";

/// A package's instrumented build. Everything lands in its own target
/// directory, so the artifacts `gate` and `measure` keep are never touched.
pub struct Instrumented<'a> {
    pkg: &'a str,
    features: Option<&'a str>,
    host: String,
    target_dir: PathBuf,
}

impl<'a> Instrumented<'a> {
    pub fn new(pkg: &'a str, features: Option<&'a str>, host: String) -> Result<Self, String> {
        let target_dir = invoke::target_dir(None)
            .map_err(|e| e.to_string())?
            .join("soothfast-coverage");
        Ok(Instrumented {
            pkg,
            features,
            host,
            target_dir,
        })
    }

    /// `cargo test` over every test target but the doc tests, which rustdoc
    /// compiles without these flags.
    pub fn tests(&self, no_run: bool) -> Command {
        let mut cmd = self.cargo("test");
        cmd.arg("--tests");
        if no_run {
            cmd.args(["--no-run", "--message-format=json"]);
        }
        cmd
    }

    /// `cargo bench` of the bench target `target` under the dev profile,
    /// which executes the same code as the bench profile, and with the
    /// runner's cheapest backend: one pass over each item.
    pub fn benches(&self, target: &str, no_run: bool) -> Command {
        let mut cmd = self.cargo("bench");
        cmd.args(["--bench", target, "--profile", "dev"]);
        if no_run {
            cmd.args(["--no-run", "--message-format=json"]);
        } else {
            cmd.args(["--", "--backend", "alloc"]);
        }
        cmd
    }

    /// The flags reach only the artifacts of an explicit `--target`, so
    /// build scripts and proc macros stay uninstrumented.
    fn cargo(&self, verb: &str) -> Command {
        let env = |k: &str| std::env::var(k).ok();
        let mut flags = existing(env, "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS");
        flags.push(FLAG.into());
        let mut cmd = Command::new("cargo");
        cmd.arg(verb).args(["-p", self.pkg, "--target", &self.host]);
        if let Some(features) = self.features {
            cmd.args(["--features", features]);
        }
        cmd.env("CARGO_TARGET_DIR", &self.target_dir);
        cmd.env("CARGO_ENCODED_RUSTFLAGS", flags.join("\u{1f}"));
        cmd
    }
}

/// The instrumented test executables, the artifacts of a `--no-run` build.
pub fn build_tests(instrumented: &Instrumented<'_>) -> Result<Vec<PathBuf>, String> {
    let mut cmd = instrumented.tests(true);
    cmd.stdout(Stdio::piped()).stderr(Stdio::inherit());
    let out = cmd.output().map_err(|e| format!("cargo: {e}"))?;
    if !out.status.success() {
        return Err("the instrumented test build failed".into());
    }
    let messages: Vec<Value> = invoke::cargo_messages(&out.stdout).collect();
    Ok(test_executables(&messages))
}

/// The instrumented executable of the bench target `target`.
pub fn build_bench(instrumented: &Instrumented<'_>, target: &str) -> Result<PathBuf, String> {
    let mut cmd = instrumented.benches(target, true);
    cmd.stdout(Stdio::piped()).stderr(Stdio::inherit());
    let out = cmd.output().map_err(|e| format!("cargo: {e}"))?;
    if !out.status.success() {
        return Err("the instrumented bench build failed".into());
    }
    invoke::bench_artifact(&out.stdout, target)
        .ok_or_else(|| format!("cargo built no bench executable named {target:?}"))
}

fn test_executables(messages: &[Value]) -> Vec<PathBuf> {
    messages
        .iter()
        .filter(|m| m["reason"] == "compiler-artifact" && m["profile"]["test"] == true)
        .filter_map(|m| m["executable"].as_str().map(PathBuf::from))
        .collect()
}

/// Run `cmd` with raw profiles written into `profraw`, which must be a
/// directory: each process adds a file of its own. A failing exit is an error
/// unless `lenient`, for a run whose failure says nothing about what it
/// executed; that run returns `false`, its profiles a lower bound.
pub fn run_into(mut cmd: Command, profraw: &Path, lenient: bool) -> Result<bool, String> {
    let dir = profraw.to_string_lossy().replace('%', "%%");
    cmd.env("LLVM_PROFILE_FILE", format!("{dir}/%m-%p.profraw"));
    let out = cmd.output().map_err(|e| format!("cargo: {e}"))?;
    if out.status.success() {
        return Ok(true);
    }
    if lenient {
        return Ok(false);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let tail: Vec<&str> = stderr.lines().rev().take(20).collect();
    Err(format!(
        "the instrumented run failed:\n{}",
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    ))
}

/// The flags the environment already sets, which the instrumented build
/// keeps: the encoded variable wins over the plain one, as in cargo.
fn existing(env: impl Fn(&str) -> Option<String>, encoded: &str, plain: &str) -> Vec<String> {
    match (env(encoded), env(plain)) {
        (Some(v), _) => v.split('\u{1f}').map(String::from).collect(),
        (None, Some(v)) => v.split_whitespace().map(String::from).collect(),
        (None, None) => Vec::new(),
    }
    .into_iter()
    .filter(|f| !f.is_empty())
    .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_failing_run_is_an_error_unless_lenient_and_then_incomplete() {
        let dir = std::env::temp_dir();
        assert_eq!(run_into(Command::new("true"), &dir, false), Ok(true));
        assert!(run_into(Command::new("false"), &dir, false).is_err());
        assert_eq!(run_into(Command::new("false"), &dir, true), Ok(false));
    }

    #[test]
    fn the_encoded_variable_wins_over_the_plain_one() {
        let env = |k: &str| match k {
            "E" => Some("-Ca\u{1f}-Cb".to_string()),
            "P" => Some("-Cc".to_string()),
            _ => None,
        };
        assert_eq!(existing(env, "E", "P"), ["-Ca", "-Cb"]);
        assert_eq!(
            existing(|k| (k == "P").then(|| "-Cc -Cd".into()), "E", "P"),
            ["-Cc", "-Cd"]
        );
        assert!(existing(|_| None, "E", "P").is_empty());
    }

    #[test]
    fn only_test_profile_artifacts_with_an_executable_are_collected() {
        let messages = [
            json!({ "reason": "compiler-artifact", "profile": { "test": true }, "executable": "/t/a" }),
            json!({ "reason": "compiler-artifact", "profile": { "test": false }, "executable": "/t/bin" }),
            json!({ "reason": "compiler-artifact", "profile": { "test": true }, "executable": null }),
            json!({ "reason": "build-finished" }),
        ];
        assert_eq!(test_executables(&messages), [PathBuf::from("/t/a")]);
    }

    #[test]
    fn the_instrumented_cargo_names_its_package_host_and_target_dir() {
        let build = Instrumented {
            pkg: "demo",
            features: Some("fast"),
            host: "x86_64-unknown-linux-gnu".into(),
            target_dir: PathBuf::from("/t/soothfast-coverage"),
        };
        let cmd = build.tests(true);
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "test",
                "-p",
                "demo",
                "--target",
                "x86_64-unknown-linux-gnu",
                "--features",
                "fast",
                "--tests",
                "--no-run",
                "--message-format=json"
            ]
        );
        let env: Vec<_> = cmd.get_envs().collect();
        assert!(env.iter().any(|(k, v)| *k == "CARGO_TARGET_DIR"
            && *v == Some(std::ffi::OsStr::new("/t/soothfast-coverage"))));
    }

    #[test]
    fn a_bench_run_takes_the_dev_profile_and_the_cheapest_backend() {
        let build = Instrumented {
            pkg: "demo",
            features: None,
            host: "h".into(),
            target_dir: PathBuf::from("/t"),
        };
        let args = |cmd: Command| -> Vec<String> {
            cmd.get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            args(build.benches("soothfast", false)),
            [
                "bench",
                "-p",
                "demo",
                "--target",
                "h",
                "--bench",
                "soothfast",
                "--profile",
                "dev",
                "--",
                "--backend",
                "alloc"
            ]
        );
        assert!(
            args(build.benches("soothfast", true))
                .ends_with(&["--no-run".to_string(), "--message-format=json".to_string()])
        );
    }
}
