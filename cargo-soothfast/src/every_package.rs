//! A command that takes one package and is given none runs once per package
//! when the root `soothfast.toml` has a `[package.NAME]` table.
//!
//! Each run is the same command re-executed with `-p NAME`, so it resolves
//! that package's own defaults. The packages are every workspace member the
//! command means something for, with or without a table of its own, in name
//! order.

use std::process::Command;

use crate::config::{Config, Group};
use crate::invoke;

/// What a package needs for a command to mean something for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Needs {
    /// A `[[bench]]` target: the one `--bench` names, else its table's, else
    /// `soothfast`.
    Bench,
    /// A `[[spec]]` entry in the package's own `soothfast.toml`.
    SpecEntry,
    /// An `[[sdk]]` entry in the package's own `soothfast.toml`.
    SdkEntry,
}

/// A command that runs per package: the defaults group its flags come from
/// and what a package needs to take part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scope {
    group: Group,
    needs: Needs,
}

/// Run `argv` (command first) once per qualifying package, or return `None`
/// when it is not a per-package command, already names a package, or no
/// package table qualifies. The caller then runs it as it always has.
pub fn dispatch(argv: &[String]) -> Option<i32> {
    let scope = scope(argv)?;
    let root = invoke::project_root();
    let config = Config::load(&root).ok()?;
    if config.package_names().ok()?.is_empty() {
        return None;
    }
    let names = crate::workspace::members(Some(&root)).ok()?;
    let packages = chosen(&names, |pkg| qualifies(&config, scope, argv, pkg));
    if packages.is_empty() {
        return None;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return Some(2);
        }
    };
    Some(run_each(argv, &packages, |args| {
        Command::new(&exe)
            .args(args)
            .status()
            .map_or(2, |s| s.code().unwrap_or(1))
    }))
}

/// The scope of `argv`, when it is a per-package command given no package.
fn scope(argv: &[String]) -> Option<Scope> {
    if argv
        .iter()
        .any(|a| matches!(a.as_str(), "-p" | "--package"))
    {
        return None;
    }
    let has = |flag: &str| argv.iter().any(|a| a == flag);
    let sub = argv.get(1).map(String::as_str);
    let (group, needs) = match (argv.first()?.as_str(), sub) {
        ("measure", _) => (Group::Gate, Needs::Bench),
        ("gate", Some("accept")) => return None,
        ("gate", _) if has("--import") => return None,
        ("gate", _) => (Group::Gate, Needs::Bench),
        ("spec" | "sdk", Some("gate")) => (Group::Spec, Needs::SpecEntry),
        ("spec", Some("gen" | "check")) => (Group::Spec, Needs::SpecEntry),
        ("sdk", Some("gen")) => (Group::Spec, Needs::SdkEntry),
        ("coverage", Some("measure")) => (Group::Docs, Needs::Bench),
        _ => return None,
    };
    Some(Scope { group, needs })
}

fn qualifies(config: &Config, scope: Scope, argv: &[String], pkg: &str) -> bool {
    match scope.needs {
        Needs::Bench => {
            let table = config.resolve(&[pkg.to_string()], scope.group);
            let target = flag_value(argv, &["--bench", "--target"])
                .or_else(|| table.ok().and_then(|s| s.target))
                .unwrap_or_else(|| "soothfast".into());
            crate::workspace::has_bench_target(pkg, &target, None).unwrap_or(false)
        }
        Needs::SpecEntry => invoke::pkg_dir(pkg)
            .is_ok_and(|dir| crate::spec_config::load(&dir).is_ok_and(|c| !c.entries.is_empty())),
        Needs::SdkEntry => invoke::pkg_dir(pkg)
            .is_ok_and(|dir| crate::sdk_config::load(&dir).is_ok_and(|c| !c.entries.is_empty())),
    }
}

fn flag_value(argv: &[String], flags: &[&str]) -> Option<String> {
    let at = argv.iter().position(|a| flags.contains(&a.as_str()))?;
    argv.get(at + 1).cloned()
}

/// The names that qualify, in order.
fn chosen(names: &[String], mut qualifies: impl FnMut(&str) -> bool) -> Vec<String> {
    names
        .iter()
        .filter(|name| qualifies(name))
        .cloned()
        .collect()
}

/// `argv` with `-p pkg` appended.
fn with_package(argv: &[String], pkg: &str) -> Vec<String> {
    let mut out = argv.to_vec();
    out.extend(["-p".into(), pkg.into()]);
    out
}

/// Run every package whatever the others exit with, each under a header
/// line, and return the worst exit code.
fn run_each(argv: &[String], packages: &[String], mut run: impl FnMut(&[String]) -> i32) -> i32 {
    let mut worst = 0;
    for pkg in packages {
        println!("soothfast: package {pkg}");
        worst = worst.max(run(&with_package(argv, pkg)));
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_package_flag_leaves_the_command_alone() {
        assert_eq!(scope(&argv(&["gate", "-p", "a"])), None);
        assert_eq!(scope(&argv(&["spec", "gen", "--package", "a"])), None);
    }

    #[test]
    fn per_package_commands_have_their_group_and_need() {
        let of = |a: &[&str]| scope(&argv(a)).map(|s| (s.group, s.needs));
        assert_eq!(of(&["measure"]), Some((Group::Gate, Needs::Bench)));
        assert_eq!(
            of(&["gate", "--against-ref", "x"]),
            Some((Group::Gate, Needs::Bench))
        );
        assert_eq!(of(&["spec", "gate"]), Some((Group::Spec, Needs::SpecEntry)));
        assert_eq!(of(&["sdk", "gate"]), Some((Group::Spec, Needs::SpecEntry)));
        assert_eq!(of(&["sdk", "gen"]), Some((Group::Spec, Needs::SdkEntry)));
        assert_eq!(
            of(&["coverage", "measure"]),
            Some((Group::Docs, Needs::Bench))
        );
    }

    #[test]
    fn commands_with_their_own_meaning_are_left_alone() {
        for a in [
            &["gate", "accept"][..],
            &["gate", "--import", "jmh", "f"],
            &["coverage", "docs"],
            &["coverage", "docs", "--dir", "d", "--lang", "py"],
            &["docs", "check"],
            &["test"],
            &["ci", "gate"],
            &["sdk", "publish"],
            &["spec", "html"],
        ] {
            assert_eq!(scope(&argv(a)), None, "{a:?}");
        }
    }

    #[test]
    fn chosen_keeps_qualifying_names_in_order() {
        let names = argv(&["a", "b", "c"]);
        assert_eq!(chosen(&names, |n| n != "b"), argv(&["a", "c"]));
        assert!(chosen(&names, |_| false).is_empty());
    }

    #[test]
    fn the_package_is_appended_to_the_command() {
        assert_eq!(
            with_package(&argv(&["gate", "--features", "f"]), "a"),
            argv(&["gate", "--features", "f", "-p", "a"])
        );
    }

    #[test]
    fn every_package_runs_after_a_failure_and_the_worst_code_wins() {
        let mut seen = Vec::new();
        let rc = run_each(&argv(&["gate"]), &argv(&["a", "b", "c"]), |args| {
            seen.push(args.last().cloned().unwrap());
            match args.last().unwrap().as_str() {
                "a" => 3,
                "b" => 1,
                _ => 0,
            }
        });
        assert_eq!(rc, 3);
        assert_eq!(seen, argv(&["a", "b", "c"]));
    }

    #[test]
    fn all_passing_packages_exit_zero() {
        assert_eq!(run_each(&argv(&["gate"]), &argv(&["a", "b"]), |_| 0), 0);
    }

    #[test]
    fn a_flag_value_is_read_under_either_spelling() {
        let a = argv(&["gate", "--target", "perf"]);
        assert_eq!(
            flag_value(&a, &["--bench", "--target"]).as_deref(),
            Some("perf")
        );
        assert_eq!(flag_value(&argv(&["gate"]), &["--bench"]), None);
    }
}
