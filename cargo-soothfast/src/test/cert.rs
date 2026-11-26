//! The run certificate: proof that a whole `cargo soothfast test` run passed
//! under inputs that have not changed since.
//!
//! A key covers the tree snapshot, `Cargo.lock`, every cargo config and
//! toolchain file cargo would read, the compiler, cargo and rustdoc versions,
//! the `CARGO_*`, `RUSTFLAGS`, `RUSTC*` and `RUSTDOC*` environment, the
//! `[test] env` values, the arguments and the selected packages.
//! Files git ignores and anything a test reads outside the repo (the network,
//! the clock, the home directory) are invisible here and can yield a stale
//! certificate. A path dependency or `[patch]` that resolves outside the repo
//! is refused outright, since its contents are not in the snapshot.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use super::config::TestConfig;
use super::key::env_values;
use super::{TestArgs, last, runner};
use crate::runcache::hashed;
use crate::{buildstamp, workspace};

const RUN_SCHEMA: &str = "soothfast-test-run-v1";

const ENV_PREFIXES: &[&str] = &["CARGO_", "RUSTFLAGS", "RUSTC", "RUSTDOC"];

/// Computes the certificate key for one run against the files as they are
/// right now.
pub struct Certifier<'a> {
    root: &'a Path,
    test_env: Vec<(String, String)>,
    args: &'a TestArgs,
    selected: &'a [String],
    certifiable: bool,
}

impl<'a> Certifier<'a> {
    /// Resolve the dependency graph once and say, once, if it reaches outside
    /// the repo.
    pub fn new(
        root: &'a Path,
        cfg: &TestConfig,
        args: &'a TestArgs,
        selected: &'a [String],
    ) -> Result<Certifier<'a>, String> {
        let meta = workspace::metadata(None, false).map_err(|e| e.to_string())?;
        let outside = outside_repo(&meta, root);
        if !outside.is_empty() {
            println!(
                "test: no stored pass is used for this run: {} resolves outside the repo, so its files are not in the key",
                outside.join(", ")
            );
        }
        Ok(Certifier {
            root,
            test_env: env_values(&cfg.env),
            args,
            selected,
            certifiable: outside.is_empty(),
        })
    }

    /// The key a certificate for this run is stored under, or `None` when
    /// the run cannot be certified.
    pub fn key(&self) -> Result<Option<String>, String> {
        let Some(snapshot) = self
            .certifiable
            .then(|| last::snapshot(self.root))
            .flatten()
        else {
            return Ok(None);
        };
        Ok(Some(run_key_of(RunParts {
            snapshot: &snapshot,
            environment: &self.environment()?,
            test_env: &self.test_env,
            cargo_args: &self.args.cargo_args,
            run_args: &self.args.run_args,
            no_doc: self.args.no_doc,
            selected: self.selected,
        })))
    }

    fn environment(&self) -> Result<String, String> {
        let cargo_home = runner::cargo_home(|k| std::env::var(k).ok());
        let mut files = runner::config_files(self.root, cargo_home);
        files.extend(toolchain_files(self.root));
        let tools = [
            buildstamp::rustc_vv(None).map_err(|e| e.to_string())?,
            version("cargo", "-V")?,
            version("rustdoc", "-vV")?,
        ];
        let lock = std::fs::read_to_string(self.root.join("Cargo.lock")).unwrap_or_default();
        Ok(environment_digest(&EnvParts {
            files: &files,
            tools: &tools,
            vars: &relevant_env(std::env::vars()),
            lock: &lock,
        }))
    }
}

fn version(program: &str, flag: &str) -> Result<String, String> {
    let out = Command::new(program)
        .arg(flag)
        .output()
        .map_err(|e| format!("{program} {flag}: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn toolchain_files(root: &Path) -> Vec<(PathBuf, String)> {
    root.ancestors()
        .flat_map(|d| [d.join("rust-toolchain.toml"), d.join("rust-toolchain")])
        .filter_map(|p| Some((p.clone(), std::fs::read_to_string(&p).ok()?)))
        .collect()
}

/// The environment variables a cargo build or run reads, sorted by name.
fn relevant_env(vars: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut kept: Vec<(String, String)> = vars
        .filter(|(k, _)| ENV_PREFIXES.iter().any(|p| k.starts_with(p)))
        .collect();
    kept.sort();
    kept
}

/// Names of the packages built from a manifest outside `root`: path
/// dependencies and `[patch]` entries, which carry no registry source.
fn outside_repo(meta: &Value, root: &Path) -> Vec<String> {
    meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["source"].is_null())
        .filter(|p| {
            p["manifest_path"]
                .as_str()
                .is_some_and(|m| !Path::new(m).starts_with(root))
        })
        .filter_map(|p| p["name"].as_str().map(String::from))
        .collect()
}

struct EnvParts<'a> {
    files: &'a [(PathBuf, String)],
    tools: &'a [String],
    vars: &'a [(String, String)],
    lock: &'a str,
}

fn environment_digest(p: &EnvParts) -> String {
    let files: Vec<String> = p
        .files
        .iter()
        .map(|(path, text)| format!("{}\u{0}{text}", path.display()))
        .collect();
    let vars: Vec<String> = p.vars.iter().map(|(k, v)| format!("{k}={v}")).collect();
    hashed(&[
        files.join("\u{2}"),
        p.tools.join("\u{2}"),
        vars.join("\u{2}"),
        p.lock.into(),
    ])
}

struct RunParts<'a> {
    snapshot: &'a str,
    environment: &'a str,
    test_env: &'a [(String, String)],
    cargo_args: &'a [String],
    run_args: &'a [String],
    no_doc: bool,
    selected: &'a [String],
}

fn run_key_of(p: RunParts) -> String {
    let test_env: Vec<String> = p.test_env.iter().map(|(n, v)| format!("{n}={v}")).collect();
    format!(
        "run-{}",
        hashed(&[
            RUN_SCHEMA.into(),
            p.snapshot.into(),
            p.environment.into(),
            test_env.join("\u{2}"),
            p.cargo_args.join("\u{2}"),
            p.run_args.join("\u{2}"),
            p.no_doc.to_string(),
            p.selected.join("\u{2}"),
        ])
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NONE: &[String] = &[];

    fn base() -> RunParts<'static> {
        RunParts {
            snapshot: "tree:0",
            environment: "env",
            test_env: &[],
            cargo_args: NONE,
            run_args: NONE,
            no_doc: false,
            selected: NONE,
        }
    }

    #[test]
    fn each_run_input_moves_the_key() {
        let one = vec!["--release".to_string()];
        let env = vec![("DATABASE_URL".to_string(), "x".to_string())];
        let moved = [
            RunParts {
                snapshot: "tree:1",
                ..base()
            },
            RunParts {
                environment: "env2",
                ..base()
            },
            RunParts {
                test_env: &env,
                ..base()
            },
            RunParts {
                cargo_args: &one,
                ..base()
            },
            RunParts {
                run_args: &one,
                ..base()
            },
            RunParts {
                no_doc: true,
                ..base()
            },
            RunParts {
                selected: &one,
                ..base()
            },
        ];
        let original = run_key_of(base());
        assert_eq!(original, run_key_of(base()));
        for parts in moved {
            assert_ne!(run_key_of(parts), original);
        }
    }

    fn env_parts<'a>() -> EnvParts<'a> {
        EnvParts {
            files: &[],
            tools: &[],
            vars: &[],
            lock: "",
        }
    }

    #[test]
    fn each_environment_input_moves_the_digest() {
        let files = [(PathBuf::from(".cargo/config.toml"), "a".to_string())];
        let tools = ["rustc 1".to_string()];
        let vars = [("RUSTFLAGS".to_string(), "-Cdebuginfo=0".to_string())];
        let moved = [
            EnvParts {
                files: &files,
                ..env_parts()
            },
            EnvParts {
                tools: &tools,
                ..env_parts()
            },
            EnvParts {
                vars: &vars,
                ..env_parts()
            },
            EnvParts {
                lock: "[[package]]",
                ..env_parts()
            },
        ];
        let original = environment_digest(&env_parts());
        for parts in moved {
            assert_ne!(environment_digest(&parts), original);
        }
    }

    #[test]
    fn only_cargo_and_compiler_variables_are_kept() {
        let vars = [
            "CARGO_TARGET_DIR",
            "RUSTFLAGS",
            "RUSTDOCFLAGS",
            "RUSTC_WRAPPER",
            "HOME",
            "PATH",
        ]
        .map(|k| (k.to_string(), "v".to_string()));
        let kept: Vec<String> = relevant_env(vars.into_iter())
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            kept,
            [
                "CARGO_TARGET_DIR",
                "RUSTC_WRAPPER",
                "RUSTDOCFLAGS",
                "RUSTFLAGS"
            ]
        );
    }

    #[test]
    fn a_path_package_outside_the_repo_is_refused() {
        let meta = json!({ "packages": [
            { "name": "inside", "source": null, "manifest_path": "/repo/a/Cargo.toml" },
            { "name": "outside", "source": null, "manifest_path": "/elsewhere/Cargo.toml" },
            { "name": "serde", "source": "registry+x", "manifest_path": "/home/.cargo/serde/Cargo.toml" },
        ]});
        assert_eq!(outside_repo(&meta, Path::new("/repo")), ["outside"]);
    }
}
