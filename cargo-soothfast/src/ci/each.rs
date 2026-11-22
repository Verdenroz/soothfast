//! What the check kinds share: one recorded command per package, and the
//! worst exit code of them.

use std::process::Command;

use super::env::CiEnv;
use super::record::run_recorded;
use crate::verdict::ADVISORY_EXIT;

/// A record's name and the arguments `cargo soothfast` runs for it.
#[derive(Debug, PartialEq)]
pub struct Job {
    pub name: String,
    pub args: Vec<String>,
}

/// Runs every job recorded as a `check`, and returns the worst exit code.
/// With no jobs it says `none` and records nothing.
pub fn run_jobs(env: &CiEnv, none: &str, jobs: &[Job]) -> i32 {
    if jobs.is_empty() {
        println!("{none}");
        return 0;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    jobs.iter()
        .map(|job| {
            let mut cmd = Command::new(&exe);
            cmd.args(&job.args);
            match run_recorded(env, &job.name, "check", &mut cmd) {
                ADVISORY_EXIT => 0,
                code => code,
            }
        })
        .max()
        .unwrap_or(0)
}

/// The packages a kind runs for: the `-p` ones given, else what `found`
/// reads from configuration.
fn named_or(
    kind: &str,
    args: &[String],
    found: impl FnOnce() -> Result<Vec<String>, String>,
) -> Result<Vec<String>, i32> {
    let mut named = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match (a.as_str(), it.next()) {
            ("-p" | "--package", Some(pkg)) => named.push(pkg.clone()),
            ("-p" | "--package", None) => {
                return Err(crate::arg_err(&format!("{a} needs a value")));
            }
            (other, _) => {
                return Err(crate::arg_err(&format!(
                    "unknown ci {kind} argument {other:?}"
                )));
            }
        }
    }
    if !named.is_empty() {
        return Ok(named);
    }
    found().map_err(|e| {
        eprintln!("soothfast: {e}");
        2
    })
}

/// Runs `command` for each package the kind covers, recorded as `KIND-PKG`.
/// `none` is what it says when there is no package to cover.
pub fn run_packages(
    env: &CiEnv,
    kind: &str,
    args: &[String],
    none: &str,
    found: impl FnOnce() -> Result<Vec<String>, String>,
    command: impl Fn(&str) -> Vec<String>,
) -> i32 {
    let packages = match named_or(kind, args, found) {
        Ok(packages) => packages,
        Err(code) => return code,
    };
    let jobs: Vec<Job> = packages
        .iter()
        .map(|pkg| Job {
            name: format!("{kind}-{pkg}"),
            args: command(pkg),
        })
        .collect();
    run_jobs(env, none, &jobs)
}

/// Runs `cargo soothfast` with `args` and returns its exit code.
pub fn spawn(args: &[String]) -> i32 {
    let status = std::env::current_exe().and_then(|exe| Command::new(exe).args(args).status());
    match status {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("soothfast: {e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::env::Flags;
    use std::collections::BTreeMap;

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn named_packages_win_over_configuration() {
        let found = || Ok(argv(&["configured"]));
        assert_eq!(
            named_or("proto", &argv(&["-p", "a", "--package", "b"]), found),
            Ok(argv(&["a", "b"]))
        );
        assert_eq!(named_or("proto", &[], found), Ok(argv(&["configured"])));
        assert_eq!(named_or("proto", &argv(&["--nope"]), found), Err(2));
        assert_eq!(named_or("proto", &argv(&["-p"]), found), Err(2));
        assert_eq!(named_or("proto", &[], || Err("boom".into())), Err(2));
    }

    #[test]
    fn nothing_to_run_passes() {
        let env = CiEnv::parse(&BTreeMap::new(), None, &Flags::default());
        assert_eq!(run_jobs(&env, "nothing", &[]), 0);
    }
}
