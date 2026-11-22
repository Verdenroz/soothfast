//! `ci spec`: the workflow's spec checks, per package: a stale generated
//! spec, a breaking change since the base, a hand-written spec that no longer
//! matches its routes, and a stale generated SDK. The gate and the reconcile
//! check are advisory unless `[ci] enforce` names `spec`.

use std::path::Path;

use super::each::{self, Job};
use super::env::{CiEnv, Event};
use super::regen;
use crate::config::Config;
use crate::spec_config::Mode;
use crate::verdict::ADVISORY_EXIT;
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

/// The checks a run skips where the bot regenerates what they compare: the
/// generated spec and the generated SDK.
fn regenerated(pkg: &Package) -> Vec<&'static str> {
    let mut names = Vec::new();
    if pkg.generates {
        names.push("spec gen --check");
    }
    if pkg.sdk {
        names.push("sdk gen --check");
    }
    names
}

fn steps(pkg: &Package, against: Option<&str>, regen: bool) -> Vec<Vec<String>> {
    let command = |words: &[&str], tail: &[&str]| -> Vec<String> {
        let mut args: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        args.extend(["-p".into(), pkg.name.clone()]);
        args.extend(tail.iter().map(|w| w.to_string()));
        args
    };
    let mut steps = Vec::new();
    if pkg.generates && !regen {
        steps.push(command(&["spec", "gen"], &["--check"]));
    }
    // The base's committed spec matches its code, so it is read instead of
    // built. HEAD's spec is always built from its routes.
    if let Some(base) = against {
        steps.push(command(
            &["spec", "gate"],
            &["--base", base, "--prefer-committed"],
        ));
    }
    if pkg.reconciles {
        steps.push(command(&["spec", "check"], &[]));
    }
    if pkg.sdk && !regen {
        steps.push(command(&["sdk", "gen"], &["--check"]));
    }
    steps
}

/// Whether exit code 1 of `step` is a finding about the API. 2 or more means
/// the command could not run.
fn finds_advisory(step: &[String]) -> bool {
    matches!(step, [a, b, ..] if a == "spec" && matches!(b.as_str(), "gate" | "check"))
}

/// Runs every step and returns the worst exit code. Unless `enforce`, a
/// finding of the gate or the reconcile check is advisory: it does not count,
/// and a run with nothing worse returns [`ADVISORY_EXIT`].
fn settle(steps: &[Vec<String>], enforce: bool, mut run: impl FnMut(&[String]) -> i32) -> i32 {
    let mut worst = 0;
    let mut advisory = Vec::new();
    for step in steps {
        let code = run(step);
        if code == 1 && !enforce && finds_advisory(step) {
            advisory.push(step);
        } else {
            worst = worst.max(code);
        }
    }
    // Printed after every step, so the record's tail keeps them.
    for step in &advisory {
        println!(
            "ci spec: the finding of `{}` is advisory and does not fail the job",
            step.join(" ")
        );
    }
    if worst == 0 && !advisory.is_empty() {
        ADVISORY_EXIT
    } else {
        worst
    }
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

fn inner(env: &CiEnv, opts: &Options) -> i32 {
    let [name] = opts.packages.as_slice() else {
        return crate::arg_err("ci spec --inner takes one -p");
    };
    let read = package(name).and_then(|pkg| {
        let regen = (pkg.generates || pkg.sdk) && regen::bot_regenerates(env)?;
        let root = invoke::workspace_root().map_err(|e| e.to_string())?;
        let enforce = Config::load(&root)?.enforces("spec")?;
        Ok((pkg, regen, enforce))
    });
    match read {
        Ok((pkg, regen, enforce)) => {
            if regen {
                for skipped in regenerated(&pkg) {
                    println!("ci spec: {skipped} skipped, regenerated on the default branch");
                }
            }
            let steps = steps(&pkg, opts.against.as_deref(), regen);
            settle(&steps, enforce, each::spawn)
        }
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
        return inner(env, &opts);
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
            steps(&pkg(true, false, false), Some("b4se"), false),
            [
                argv(&["spec", "gen", "-p", "api", "--check"]),
                argv(&[
                    "spec",
                    "gate",
                    "-p",
                    "api",
                    "--base",
                    "b4se",
                    "--prefer-committed"
                ]),
            ]
        );
    }

    #[test]
    fn each_step_runs_only_when_its_input_is_configured() {
        assert_eq!(
            steps(&pkg(false, true, true), None, false),
            [
                argv(&["spec", "check", "-p", "api"]),
                argv(&["sdk", "gen", "-p", "api", "--check"]),
            ]
        );
        assert!(steps(&pkg(false, false, false), None, false).is_empty());
    }

    #[test]
    fn regeneration_drops_the_checks_the_bot_makes_current() {
        assert_eq!(
            steps(&pkg(true, true, true), Some("b4se"), true),
            [
                argv(&[
                    "spec",
                    "gate",
                    "-p",
                    "api",
                    "--base",
                    "b4se",
                    "--prefer-committed"
                ]),
                argv(&["spec", "check", "-p", "api"]),
            ]
        );
        assert_eq!(
            steps(&pkg(true, true, true), Some("b4se"), false).len(),
            4,
            "regeneration off keeps every step"
        );
    }

    #[test]
    fn a_package_with_nothing_left_runs_no_step_and_names_what_it_skipped() {
        assert!(steps(&pkg(true, false, true), None, true).is_empty());
        assert_eq!(steps(&pkg(true, false, false), Some("b4se"), true).len(), 1);
        assert_eq!(
            regenerated(&pkg(true, true, true)),
            ["spec gen --check", "sdk gen --check"]
        );
        assert_eq!(regenerated(&pkg(false, true, false)), Vec::<&str>::new());
    }

    fn finding_on(step: &str, code: i32) -> impl FnMut(&[String]) -> i32 + '_ {
        move |args| if args[1] == step { code } else { 0 }
    }

    fn all_steps() -> Vec<Vec<String>> {
        steps(&pkg(true, true, true), Some("b4se"), false)
    }

    #[test]
    fn a_gate_finding_is_advisory_by_default() {
        let found = settle(&all_steps(), false, finding_on("gate", 1));
        assert_eq!(found, ADVISORY_EXIT);
        let reconcile = settle(&all_steps(), false, finding_on("check", 1));
        assert_eq!(reconcile, ADVISORY_EXIT);
        assert_eq!(settle(&all_steps(), false, |_| 0), 0);
    }

    #[test]
    fn an_enforced_finding_fails_as_before() {
        assert_eq!(settle(&all_steps(), true, finding_on("gate", 1)), 1);
        assert_eq!(settle(&all_steps(), true, finding_on("check", 1)), 1);
    }

    #[test]
    fn a_step_that_could_not_run_fails_with_or_without_enforce() {
        for enforce in [false, true] {
            assert_eq!(settle(&all_steps(), enforce, finding_on("gate", 2)), 2);
            assert_eq!(settle(&all_steps(), enforce, finding_on("check", 2)), 2);
        }
    }

    #[test]
    fn every_step_runs_after_one_fails() {
        let mut ran = Vec::new();
        let code = settle(&all_steps(), false, |args| {
            ran.push(args[..2].join(" "));
            2
        });
        assert_eq!(code, 2);
        assert_eq!(ran, ["spec gen", "spec gate", "spec check", "sdk gen"]);
    }

    #[test]
    fn a_stale_generated_file_is_never_advisory() {
        assert_eq!(settle(&all_steps(), false, finding_on("gen", 1)), 1);
        let both = |args: &[String]| if args[1] == "gate" { 1 } else { 2 };
        assert_eq!(settle(&all_steps(), false, both), 2);
        let mixed = |args: &[String]| {
            if args[0] == "sdk" || args[1] == "gate" {
                1
            } else {
                0
            }
        };
        assert_eq!(settle(&all_steps(), false, mixed), 1);
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
