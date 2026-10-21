//! Whether the run step can install the shim as cargo's target runner.
//!
//! `--config target.<triple>.runner` replaces a runner the user configured,
//! so one set in the environment is chained through the shim. A runner, or a
//! default build target, set in a config file falls back to running each
//! binary on its own: resolving cargo's config layering here would drift from
//! cargo's own.

use std::path::{Path, PathBuf};

/// How the run step treats the target runner.
#[derive(Debug, PartialEq)]
pub enum Runner {
    /// The shim runs each binary itself, optionally through a chained runner.
    Shim(Vec<String>),
    /// Each binary runs through its own `cargo test`, for the given reason.
    Fallback(String),
}

/// The triples the run step sets a runner for: the host, every `--target`,
/// and `CARGO_BUILD_TARGET`.
pub fn triples(host: &str, cargo_args: &[String], build_target: Option<String>) -> Vec<String> {
    let mut out = vec![host.to_string()];
    let mut it = cargo_args.iter();
    while let Some(a) = it.next() {
        let target = match a.strip_prefix("--target=") {
            Some(t) => Some(t.to_string()),
            None if a == "--target" => it.next().cloned(),
            None => None,
        };
        out.extend(target);
    }
    out.extend(build_target);
    out.sort();
    out.dedup();
    out
}

/// `CARGO_TARGET_<TRIPLE>_RUNNER`.
pub fn runner_var(triple: &str) -> String {
    let triple: String = triple
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("CARGO_TARGET_{triple}_RUNNER")
}

/// Decide from the `--config` arguments, the runner variables (`env`, by
/// name) and the config files cargo would read.
pub fn detect(
    triples: &[String],
    cargo_args: &[String],
    env: impl Fn(&str) -> Option<String>,
    config_files: &[(PathBuf, String)],
) -> Runner {
    for value in config_values(cargo_args) {
        if !value.contains('=') {
            return Runner::Fallback(format!("--config {value} names a config file"));
        }
        if let Some(key) = config_key(value) {
            return Runner::Fallback(format!("--config {value} sets `{key}`"));
        }
    }
    for (path, text) in config_files {
        if let Some(key) = config_key(text) {
            return Runner::Fallback(format!("{} sets `{key}`", path.display()));
        }
    }
    let runners: Vec<Option<String>> = triples.iter().map(|t| env(&runner_var(t))).collect();
    match runners.first() {
        Some(first) if runners.iter().all(|r| r == first) => Runner::Shim(
            first
                .as_deref()
                .map(|r| r.split_whitespace().map(String::from).collect())
                .unwrap_or_default(),
        ),
        _ => Runner::Fallback("the targets have different runners".into()),
    }
}

/// The values given to `--config`, spelled either way.
fn config_values(cargo_args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut it = cargo_args.iter();
    while let Some(a) = it.next() {
        match a.strip_prefix("--config=") {
            Some(v) => out.push(v),
            None if a == "--config" => out.extend(it.next().map(String::as_str)),
            None => {}
        }
    }
    out
}

/// A config key the shim cannot account for: a runner, or `build.target`,
/// which adds a triple the run step would not set a runner for.
fn config_key(text: &str) -> Option<&'static str> {
    let keys = text
        .lines()
        .filter_map(|l| l.split('#').next()?.split_once('='))
        .map(|(k, _)| k.trim());
    for key in keys {
        if key == "runner" || key.ends_with(".runner") {
            return Some("runner");
        }
        if key == "target" || key == "build.target" {
            return Some("target");
        }
    }
    None
}

/// `CARGO_HOME`, else the `.cargo` directory in the user's home, where cargo
/// reads its global config whether or not the workspace sits under it.
pub fn cargo_home(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    env("CARGO_HOME").map(PathBuf::from).or_else(|| {
        env("HOME")
            .or_else(|| env("USERPROFILE"))
            .map(|h| PathBuf::from(h).join(".cargo"))
    })
}

/// Every config file cargo reads from `root`: each ancestor's `.cargo`
/// directory, then the cargo home.
pub fn config_files(root: &Path, cargo_home: Option<PathBuf>) -> Vec<(PathBuf, String)> {
    let dirs = root.ancestors().map(|d| d.join(".cargo")).chain(cargo_home);
    dirs.flat_map(|d| [d.join("config.toml"), d.join("config")])
        .filter_map(|p| Some((p.clone(), std::fs::read_to_string(&p).ok()?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn the_host_and_every_requested_target_get_a_runner() {
        let t = triples(
            "x86_64-unknown-linux-gnu",
            &args(&[
                "--target",
                "aarch64-apple-darwin",
                "--target=x86_64-unknown-linux-gnu",
            ]),
            Some("wasm32-wasip1".into()),
        );
        assert_eq!(
            t,
            [
                "aarch64-apple-darwin",
                "wasm32-wasip1",
                "x86_64-unknown-linux-gnu"
            ]
        );
    }

    #[test]
    fn the_runner_variable_is_the_triple_upper_cased() {
        assert_eq!(
            runner_var("x86_64-unknown-linux-gnu"),
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER"
        );
    }

    #[test]
    fn with_no_runner_the_shim_runs_binaries_directly() {
        let t = args(&["x86_64-unknown-linux-gnu"]);
        assert_eq!(detect(&t, &[], no_env, &[]), Runner::Shim(Vec::new()));
    }

    #[test]
    fn a_runner_in_the_environment_is_chained() {
        let t = args(&["x86_64-unknown-linux-gnu"]);
        let env = |k: &str| (k == runner_var(&t[0])).then(|| "valgrind -q".to_string());
        assert_eq!(
            detect(&t, &[], env, &[]),
            Runner::Shim(args(&["valgrind", "-q"]))
        );
    }

    #[test]
    fn targets_with_different_runners_fall_back() {
        let t = args(&["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]);
        let env = |k: &str| (k == runner_var(&t[1])).then(|| "qemu".to_string());
        assert!(matches!(detect(&t, &[], env, &[]), Runner::Fallback(_)));
    }

    #[test]
    fn a_runner_or_build_target_in_a_config_file_falls_back() {
        let t = args(&["x86_64-unknown-linux-gnu"]);
        for text in [
            "[target.x86_64-unknown-linux-gnu]\nrunner = \"qemu\"\n",
            "[target.'cfg(unix)']\nrunner = [\"sudo\"]\n",
            "target.x86_64-unknown-linux-gnu.runner = \"qemu\"\n",
            "[build]\ntarget = \"wasm32-wasip1\"\n",
        ] {
            let files = [(PathBuf::from("/ws/.cargo/config.toml"), text.to_string())];
            assert!(
                matches!(detect(&t, &[], no_env, &files), Runner::Fallback(_)),
                "{text}"
            );
        }
    }

    #[test]
    fn unrelated_config_and_commented_keys_keep_the_shim() {
        let t = args(&["x86_64-unknown-linux-gnu"]);
        let text = "[env]\nRUST_LOG = \"info\"\n# runner = \"qemu\"\n[build]\njobs = 2\n";
        let files = [(PathBuf::from("/ws/.cargo/config.toml"), text.to_string())];
        assert_eq!(detect(&t, &[], no_env, &files), Runner::Shim(Vec::new()));
    }

    #[test]
    fn a_runner_given_with_config_on_the_command_line_falls_back() {
        let t = args(&["x86_64-unknown-linux-gnu"]);
        let cargo_args = args(&["--config", "target.x86_64-unknown-linux-gnu.runner='qemu'"]);
        assert!(matches!(
            detect(&t, &cargo_args, no_env, &[]),
            Runner::Fallback(_)
        ));
        let file = args(&["--config=ci/cargo.toml"]);
        assert!(matches!(
            detect(&t, &file, no_env, &[]),
            Runner::Fallback(_)
        ));
    }

    #[test]
    fn a_feature_named_runner_keeps_the_shim() {
        let t = args(&["x86_64-unknown-linux-gnu"]);
        let cargo_args = args(&["--features", "soothfast/runner", "--config", "build.jobs=2"]);
        assert_eq!(
            detect(&t, &cargo_args, no_env, &[]),
            Runner::Shim(Vec::new())
        );
    }

    #[test]
    fn the_cargo_home_defaults_to_the_users_home() {
        let env = |k: &str| (k == "HOME").then(|| "/home/u".to_string());
        assert_eq!(cargo_home(env), Some(PathBuf::from("/home/u/.cargo")));
        let set = |k: &str| (k == "CARGO_HOME").then(|| "/opt/cargo".to_string());
        assert_eq!(cargo_home(set), Some(PathBuf::from("/opt/cargo")));
    }

    #[test]
    fn a_home_runner_falls_back_for_a_workspace_outside_home() {
        let base = std::env::temp_dir().join(format!("soothfast-home-{}", std::process::id()));
        let (root, home) = (base.join("srv/ws"), base.join("home/.cargo"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[target.'cfg(unix)']\nrunner = \"sudo\"\n",
        )
        .unwrap();
        let files = config_files(&root, Some(home));
        std::fs::remove_dir_all(&base).unwrap();
        let t = args(&["x86_64-unknown-linux-gnu"]);
        assert!(matches!(
            detect(&t, &[], no_env, &files),
            Runner::Fallback(_)
        ));
    }
}
