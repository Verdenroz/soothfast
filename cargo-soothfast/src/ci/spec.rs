//! `ci spec`: the workflow's spec checks, per package: a stale generated
//! spec, a breaking change since the base, a hand-written spec that no longer
//! matches its routes, and a stale generated SDK.

use std::path::Path;

use super::each::{self, Job};
use super::env::{CiEnv, Event};
use crate::spec_config::Mode;
use crate::{invoke, sdk_config, spec_config};

/// What `ci spec` takes on the command line. `inner` and `against` are how a
/// job runs itself unrecorded, so its steps land in one record.
#[derive(Debug, Default, PartialEq)]
struct Options {
    packages: Vec<String>,
    inner: bool,
    against: Option<String>,
}

/// What a package's configuration asks for.
#[derive(Debug, Default, PartialEq)]
struct Package {
    name: String,
    generates: bool,
    reconciles: bool,
    sdk: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut opts = Options::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "-p" | "--package" => opts.packages.push(value()?),
            "--inner" => opts.inner = true,
            "--against" => opts.against = Some(value()?),
            other => return Err(format!("unknown ci spec argument {other:?}")),
        }
    }
    Ok(opts)
}

/// A pull request compares with its base, and so does any run given `--base`.
/// A push alone has nothing to compare with.
fn gate_base(env: &CiEnv) -> Option<String> {
    let given = env.event == Event::PullRequest || env.base_given;
    env.base.clone().filter(|_| given)
}

fn steps(pkg: &Package, against: Option<&str>) -> Vec<Vec<String>> {
    let command = |words: &[&str], tail: &[&str]| -> Vec<String> {
        let mut args: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        args.extend(["-p".into(), pkg.name.clone()]);
        args.extend(tail.iter().map(|w| w.to_string()));
        args
    };
    let mut steps = Vec::new();
    if pkg.generates {
        steps.push(command(&["spec", "gen"], &["--check"]));
    }
    if let Some(base) = against {
        steps.push(command(&["spec", "gate"], &["--base", base]));
    }
    if pkg.reconciles {
        steps.push(command(&["spec", "check"], &[]));
    }
    if pkg.sdk {
        steps.push(command(&["sdk", "gen"], &["--check"]));
    }
    steps
}

fn jobs(packages: &[String], against: Option<&str>) -> Vec<Job> {
    packages
        .iter()
        .map(|pkg| {
            let mut args: Vec<String> = ["ci", "spec", "-p", pkg, "--inner"]
                .map(String::from)
                .to_vec();
            if let Some(base) = against {
                args.extend(["--against".into(), base.into()]);
            }
            Job {
                name: format!("spec-{pkg}"),
                args,
            }
        })
        .collect()
}

fn package(name: &str) -> Result<Package, String> {
    let dir = invoke::pkg_dir(name).map_err(|e| e.to_string())?;
    let specs = spec_config::load(&dir)?;
    Ok(Package {
        name: name.into(),
        generates: specs.generates(),
        reconciles: specs.entries.iter().any(|e| e.mode == Mode::Check),
        sdk: !sdk_config::load(&dir)?.entries.is_empty(),
    })
}

pub(super) fn configured_at(root: &Path) -> Result<Vec<String>, String> {
    Ok(spec_config::configured(root)?.0)
}

fn configured() -> Result<Vec<String>, String> {
    configured_at(&invoke::workspace_root().map_err(|e| e.to_string())?)
}

fn inner(opts: &Options) -> i32 {
    let [name] = opts.packages.as_slice() else {
        return crate::arg_err("ci spec --inner takes one -p");
    };
    match package(name) {
        Ok(pkg) => each::run_steps(&steps(&pkg, opts.against.as_deref()), each::spawn),
        Err(e) => {
            eprintln!("soothfast: {e}");
            2
        }
    }
}

/// Entry point for `ci spec`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let opts = match parse(args) {
        Ok(opts) => opts,
        Err(e) => return crate::arg_err(&e),
    };
    if opts.inner {
        return inner(&opts);
    }
    let packages = match opts.packages {
        named if !named.is_empty() => named,
        _ => match configured() {
            Ok(found) => found,
            Err(e) => {
                eprintln!("soothfast: {e}");
                return 2;
            }
        },
    };
    let jobs = jobs(&packages, gate_base(env).as_deref());
    each::run_jobs(env, "ci spec: no package has a [[spec]] entry", &jobs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::env::Flags;
    use std::collections::BTreeMap;

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn env(event: Event, base: Option<&str>, given: bool) -> CiEnv {
        let flags = Flags {
            event: Some(event),
            base: base.filter(|_| given).map(String::from),
            results: None,
        };
        CiEnv {
            base: base.map(String::from),
            ..CiEnv::parse(&BTreeMap::new(), None, &flags)
        }
    }

    fn pkg(generates: bool, reconciles: bool, sdk: bool) -> Package {
        Package {
            name: "api".into(),
            generates,
            reconciles,
            sdk,
        }
    }

    #[test]
    fn a_pull_request_gates_against_its_base() {
        let pr = env(Event::PullRequest, Some("b4se"), false);
        assert_eq!(gate_base(&pr).as_deref(), Some("b4se"));
    }

    #[test]
    fn a_push_gates_only_against_a_base_it_was_given() {
        let push = env(Event::Push, Some("pr3v"), false);
        assert_eq!(gate_base(&push), None);
        let given = env(Event::Push, Some("mine"), true);
        assert_eq!(gate_base(&given).as_deref(), Some("mine"));
        assert_eq!(gate_base(&env(Event::Other, None, false)), None);
    }

    #[test]
    fn a_generating_package_checks_its_spec_and_gates() {
        assert_eq!(
            steps(&pkg(true, false, false), Some("b4se")),
            [
                argv(&["spec", "gen", "-p", "api", "--check"]),
                argv(&["spec", "gate", "-p", "api", "--base", "b4se"]),
            ]
        );
    }

    #[test]
    fn each_step_runs_only_when_its_input_is_configured() {
        assert_eq!(
            steps(&pkg(false, true, true), None),
            [
                argv(&["spec", "check", "-p", "api"]),
                argv(&["sdk", "gen", "-p", "api", "--check"]),
            ]
        );
        assert!(steps(&pkg(false, false, false), None).is_empty());
    }

    #[test]
    fn a_job_reruns_itself_with_the_base_it_gates_against() {
        let found = jobs(&argv(&["a", "b"]), Some("b4se"));
        assert_eq!(found[1].name, "spec-b");
        assert_eq!(
            found[1].args,
            argv(&["ci", "spec", "-p", "b", "--inner", "--against", "b4se"])
        );
        assert_eq!(
            jobs(&argv(&["a"]), None)[0].args,
            argv(&["ci", "spec", "-p", "a", "--inner"])
        );
    }

    #[test]
    fn arguments_parse() {
        let opts = parse(&argv(&["-p", "a", "--inner", "--against", "x"])).unwrap();
        assert_eq!(
            opts,
            Options {
                packages: argv(&["a"]),
                inner: true,
                against: Some("x".into()),
            }
        );
        assert!(parse(&argv(&["--nope"])).is_err());
        assert!(parse(&argv(&["-p"])).is_err());
    }
}
