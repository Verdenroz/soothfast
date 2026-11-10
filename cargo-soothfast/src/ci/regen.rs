//! `ci regen`: regenerates the derived files a default-branch push lands and
//! prints which paths to land. It never commits or pushes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::env::CiEnv;
use crate::config::Config;
use crate::{invoke, sdk_config, spec_config, workspace};

const DEFAULT_BASELINE: &str = "base";
const SPEC_PATHS: [&str; 3] = ["*.yaml", "*.yml", "*.json"];

/// What the repo configures for regeneration.
#[derive(Debug, Default, PartialEq)]
struct Inputs {
    baseline: String,
    changelog: bool,
    gated: Vec<String>,
    changelog_packages: Vec<String>,
    specs: Vec<String>,
    sdks: Vec<Sdk>,
    hook: Option<Hook>,
}

/// A package with `[[sdk]]` entries and the paths they write, from the
/// repository root.
#[derive(Debug, PartialEq)]
struct Sdk {
    package: String,
    paths: Vec<String>,
}

/// The adopter's own regeneration command and the pathspecs it may change.
#[derive(Debug, PartialEq)]
struct Hook {
    run: String,
    paths: Vec<String>,
}

#[derive(Debug, PartialEq)]
enum Step {
    Measure(String),
    Changelog(Vec<String>),
    SpecGen(String),
    SdkGen(String),
    Hook(String),
}

/// How regeneration touches the machine: the CLI's own commands, the hook,
/// and git.
trait Host {
    fn cli(&self, args: &[String]) -> i32;
    fn hook(&self, script: &str) -> i32;
    fn latest_tag(&self) -> Option<String>;
    fn changed(&self, paths: &[String]) -> bool;
}

/// Entry point for `ci regen`: `[--paths-out FILE] [--baseline NAME]
/// [--no-changelog]`.
pub fn run(_env: &CiEnv, args: &[String]) -> i32 {
    let flags = match parse(args) {
        Ok(flags) => flags,
        Err(e) => return crate::arg_err(&e),
    };
    let vars: BTreeMap<String, String> = std::env::vars().collect();
    let inputs = match discover(&flags, &vars) {
        Ok(inputs) => inputs,
        Err(e) => return crate::arg_err(&e),
    };
    let Some(report) = regenerate(&inputs, &Process) else {
        return 1;
    };
    print!("{report}");
    match flags.paths_out {
        Some(file) => match std::fs::write(&file, &report) {
            Ok(()) => 0,
            Err(e) => crate::arg_err(&format!("{}: {e}", file.display())),
        },
        None => 0,
    }
}

#[derive(Debug, PartialEq)]
struct Flags {
    paths_out: Option<PathBuf>,
    baseline: String,
    changelog: bool,
}

fn parse(args: &[String]) -> Result<Flags, String> {
    let mut flags = Flags {
        paths_out: None,
        baseline: DEFAULT_BASELINE.to_string(),
        changelog: true,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "--paths-out" => flags.paths_out = Some(PathBuf::from(value()?)),
            "--baseline" => flags.baseline = value()?,
            "--no-changelog" => flags.changelog = false,
            other => return Err(format!("unknown regen argument {other:?}")),
        }
    }
    Ok(flags)
}

fn discover(flags: &Flags, vars: &BTreeMap<String, String>) -> Result<Inputs, String> {
    let root = invoke::workspace_root().map_err(|e| e.to_string())?;
    let changelog_packages = Config::load(&root)?.changelog()?.packages;
    let gated = workspace::bench_crates("soothfast", Some(&root))?;
    let members = workspace::members(Some(&root)).map_err(|e| e.to_string())?;
    let mut specs = Vec::new();
    let mut sdks = Vec::new();
    for package in members {
        if spec_config::generating(&package).is_some() {
            specs.push(package.clone());
        }
        let dir = invoke::pkg_dir(&package).map_err(|e| e.to_string())?;
        let paths = sdk_paths(&root, &dir, &sdk_config::load(&dir)?);
        if !paths.is_empty() {
            sdks.push(Sdk { package, paths });
        }
    }
    let hook = hook(vars)?;
    Ok(Inputs {
        baseline: flags.baseline.clone(),
        changelog: flags.changelog,
        gated,
        changelog_packages,
        specs,
        sdks,
        hook,
    })
}

fn sdk_paths(root: &Path, dir: &Path, cfg: &sdk_config::SdkConfig) -> Vec<String> {
    let rel = dir.strip_prefix(root).unwrap_or(dir);
    cfg.entries
        .iter()
        .map(|e| rel.join(&e.out).display().to_string())
        .collect()
}

fn hook(vars: &BTreeMap<String, String>) -> Result<Option<Hook>, String> {
    let var = |k: &str| vars.get(k).filter(|v| !v.is_empty());
    let Some(run) = var("REGEN_RUN") else {
        return Ok(None);
    };
    let paths =
        var("REGEN_PATHS").ok_or("REGEN_RUN needs REGEN_PATHS, or nothing it changes can land")?;
    Ok(Some(Hook {
        run: run.clone(),
        paths: paths.split_whitespace().map(String::from).collect(),
    }))
}

/// The steps in order, and the pathspecs they may change.
fn plan(inputs: &Inputs) -> (Vec<Step>, Vec<String>) {
    let mut steps = Vec::new();
    let mut paths = Vec::new();
    let listed = match inputs.changelog_packages.is_empty() {
        true => &inputs.gated,
        false => &inputs.changelog_packages,
    };
    if inputs.changelog && !listed.is_empty() {
        steps.extend(inputs.gated.iter().cloned().map(Step::Measure));
        steps.push(Step::Changelog(listed.clone()));
        paths.push("CHANGELOG.md".to_string());
    }
    if !inputs.specs.is_empty() {
        steps.extend(inputs.specs.iter().cloned().map(Step::SpecGen));
        paths.extend(SPEC_PATHS.map(String::from));
    }
    for sdk in &inputs.sdks {
        steps.push(Step::SdkGen(sdk.package.clone()));
        paths.extend(sdk.paths.iter().cloned());
    }
    if let Some(hook) = &inputs.hook {
        steps.push(Step::Hook(hook.run.clone()));
        paths.extend(hook.paths.iter().cloned());
    }
    (steps, paths)
}

/// Runs the plan and returns the `paths=` and `changed=` lines the landing
/// step reads, or `None` when a step failed.
fn regenerate(inputs: &Inputs, host: &impl Host) -> Option<String> {
    let (steps, paths) = plan(inputs);
    for step in &steps {
        if execute(step, &inputs.baseline, host) != 0 {
            return None;
        }
    }
    let changed = !paths.is_empty() && host.changed(&paths);
    Some(format!("paths={}\nchanged={changed}\n", paths.join(" ")))
}

fn execute(step: &Step, baseline: &str, host: &impl Host) -> i32 {
    let words = |list: &[&str]| -> Vec<String> { list.iter().map(|s| s.to_string()).collect() };
    match step {
        Step::Measure(pkg) => {
            host.cli(&words(&["measure", "-p", pkg, "--save-baseline", baseline]))
        }
        Step::Changelog(pkgs) => {
            let mut args = words(&["report", "changelog"]);
            for pkg in pkgs {
                args.extend(words(&["-p", pkg]));
            }
            if let Some(tag) = host.latest_tag() {
                args.extend(words(&["--against-ref", &tag]));
            }
            args.extend(words(&["--baseline", baseline]));
            host.cli(&args)
        }
        Step::SpecGen(pkg) => host.cli(&words(&["spec", "gen", "-p", pkg])),
        Step::SdkGen(pkg) => host.cli(&words(&["sdk", "gen", "-p", pkg])),
        Step::Hook(script) => host.hook(script),
    }
}

struct Process;

impl Host for Process {
    fn cli(&self, args: &[String]) -> i32 {
        match std::env::current_exe() {
            Ok(exe) => status(Command::new(exe).args(args)),
            Err(e) => {
                eprintln!("soothfast: {e}");
                2
            }
        }
    }

    fn hook(&self, script: &str) -> i32 {
        let Ok(exe) = std::env::current_exe() else {
            return 2;
        };
        let mut cmd = Command::new("bash");
        cmd.args(["-euo", "pipefail", "-c", script])
            .env("SOOTHFAST", exe);
        status(&mut cmd)
    }

    fn latest_tag(&self) -> Option<String> {
        let out = Command::new("git")
            .args(["describe", "--tags", "--abbrev=0"])
            .output()
            .ok()?;
        let tag = String::from_utf8(out.stdout).ok()?;
        Some(tag.trim().to_string()).filter(|t| out.status.success() && !t.is_empty())
    }

    fn changed(&self, paths: &[String]) -> bool {
        Command::new("git")
            .args(["status", "--porcelain", "--"])
            .args(paths)
            .output()
            .is_ok_and(|o| !o.stdout.is_empty())
    }
}

fn status(cmd: &mut Command) -> i32 {
    match cmd.status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("soothfast: {e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    struct Fake {
        calls: RefCell<Vec<String>>,
        tag: Option<&'static str>,
        fail_on: Option<&'static str>,
        changed: bool,
        asked_about: RefCell<Vec<String>>,
        hook_ran: Cell<bool>,
    }

    impl Fake {
        fn new() -> Fake {
            Fake {
                calls: RefCell::new(Vec::new()),
                tag: None,
                fail_on: None,
                changed: false,
                asked_about: RefCell::new(Vec::new()),
                hook_ran: Cell::new(false),
            }
        }

        fn record(&self, call: String) -> i32 {
            let code = i32::from(self.fail_on.is_some_and(|f| call.starts_with(f)));
            self.calls.borrow_mut().push(call);
            code
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl Host for Fake {
        fn cli(&self, args: &[String]) -> i32 {
            self.record(args.join(" "))
        }

        fn hook(&self, script: &str) -> i32 {
            self.hook_ran.set(true);
            self.record(format!("hook {script}"))
        }

        fn latest_tag(&self) -> Option<String> {
            self.tag.map(String::from)
        }

        fn changed(&self, paths: &[String]) -> bool {
            *self.asked_about.borrow_mut() = paths.to_vec();
            self.changed
        }
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn inputs() -> Inputs {
        Inputs {
            baseline: "base".into(),
            changelog: true,
            ..Inputs::default()
        }
    }

    fn everything() -> Inputs {
        Inputs {
            gated: strings(&["core", "docs", "tools"]),
            changelog_packages: strings(&["core", "docs"]),
            specs: strings(&["api"]),
            sdks: vec![Sdk {
                package: "api".into(),
                paths: strings(&["api/sdk/python"]),
            }],
            hook: Some(Hook {
                run: "make gen".into(),
                paths: strings(&["gen", "extra.txt"]),
            }),
            ..inputs()
        }
    }

    #[test]
    fn nothing_configured_runs_nothing_and_changes_nothing() {
        let host = Fake::new();
        let report = regenerate(&inputs(), &host).unwrap();
        assert_eq!(report, "paths=\nchanged=false\n");
        assert!(host.calls().is_empty());
        assert!(host.asked_about.borrow().is_empty());
    }

    #[test]
    fn each_step_runs_only_when_its_input_is_configured() {
        let only = |inputs: Inputs| plan(&inputs).0;
        assert_eq!(
            only(Inputs {
                gated: strings(&["core", "tools"]),
                changelog_packages: strings(&["core"]),
                ..inputs()
            }),
            [
                Step::Measure("core".into()),
                Step::Measure("tools".into()),
                Step::Changelog(strings(&["core"]))
            ]
        );
        assert_eq!(
            only(Inputs {
                specs: strings(&["api"]),
                ..inputs()
            }),
            [Step::SpecGen("api".into())]
        );
        assert_eq!(
            only(Inputs {
                sdks: vec![Sdk {
                    package: "api".into(),
                    paths: strings(&["sdk"])
                }],
                ..inputs()
            }),
            [Step::SdkGen("api".into())]
        );
        assert_eq!(
            only(Inputs {
                hook: Some(Hook {
                    run: "x".into(),
                    paths: strings(&["p"])
                }),
                ..inputs()
            }),
            [Step::Hook("x".into())]
        );
    }

    #[test]
    fn steps_run_in_order_with_the_hook_after_the_sdks() {
        let host = Fake {
            tag: Some("v1.2.0"),
            ..Fake::new()
        };
        regenerate(&everything(), &host).unwrap();
        assert_eq!(
            host.calls(),
            [
                "measure -p core --save-baseline base",
                "measure -p docs --save-baseline base",
                "measure -p tools --save-baseline base",
                "report changelog -p core -p docs --against-ref v1.2.0 --baseline base",
                "spec gen -p api",
                "sdk gen -p api",
                "hook make gen",
            ]
        );
    }

    #[test]
    fn a_changelog_with_no_tag_lists_the_initial_surface() {
        let host = Fake::new();
        let inputs = Inputs {
            gated: strings(&["core"]),
            baseline: "main".into(),
            ..inputs()
        };
        regenerate(&inputs, &host).unwrap();
        assert_eq!(host.calls()[1], "report changelog -p core --baseline main");
    }

    #[test]
    fn the_changelog_covers_the_gated_packages_unless_it_lists_its_own() {
        let gated = Inputs {
            gated: strings(&["core", "tools"]),
            ..inputs()
        };
        assert_eq!(
            plan(&gated).0.last(),
            Some(&Step::Changelog(strings(&["core", "tools"])))
        );
        let listed = Inputs {
            changelog_packages: strings(&["core"]),
            ..gated
        };
        assert_eq!(
            plan(&listed).0.last(),
            Some(&Step::Changelog(strings(&["core"])))
        );
    }

    #[test]
    fn no_changelog_skips_measuring_and_the_changelog() {
        let off = Inputs {
            changelog: false,
            ..everything()
        };
        let (steps, paths) = plan(&off);
        assert!(
            !steps
                .iter()
                .any(|s| matches!(s, Step::Measure(_) | Step::Changelog(_)))
        );
        assert!(!paths.contains(&"CHANGELOG.md".to_string()));
    }

    #[test]
    fn a_repository_with_nothing_gated_or_listed_has_no_changelog_step() {
        assert!(plan(&inputs()).0.is_empty());
        let listed = Inputs {
            changelog_packages: strings(&["core"]),
            ..inputs()
        };
        assert_eq!(plan(&listed).0, [Step::Changelog(strings(&["core"]))]);
    }

    #[test]
    fn the_changed_paths_are_printed_for_the_landing_step() {
        let host = Fake {
            changed: true,
            ..Fake::new()
        };
        let report = regenerate(&everything(), &host).unwrap();
        assert_eq!(
            report,
            "paths=CHANGELOG.md *.yaml *.yml *.json api/sdk/python gen extra.txt\nchanged=true\n"
        );
        assert_eq!(host.asked_about.borrow().len(), 7);
    }

    #[test]
    fn a_failing_step_stops_the_run_and_reports_nothing() {
        let host = Fake {
            fail_on: Some("spec gen"),
            ..Fake::new()
        };
        assert_eq!(regenerate(&everything(), &host), None);
        assert!(!host.hook_ran.get());
        assert_eq!(host.calls().last().unwrap(), "spec gen -p api");
    }

    #[test]
    fn the_hook_needs_paths_to_land() {
        let vars = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(hook(&vars(&[])), Ok(None));
        assert!(hook(&vars(&[("REGEN_RUN", "make")])).is_err());
        let set = hook(&vars(&[("REGEN_RUN", "make"), ("REGEN_PATHS", "a  b")]));
        assert_eq!(
            set,
            Ok(Some(Hook {
                run: "make".into(),
                paths: strings(&["a", "b"])
            }))
        );
    }

    #[test]
    fn arguments_take_a_paths_file_a_baseline_and_no_changelog() {
        let flags = |paths_out: Option<&str>, baseline: &str, changelog| Flags {
            paths_out: paths_out.map(Into::into),
            baseline: baseline.into(),
            changelog,
        };
        assert_eq!(parse(&[]), Ok(flags(None, "base", true)));
        let args = strings(&["--paths-out", "out.txt", "--baseline", "main"]);
        assert_eq!(parse(&args), Ok(flags(Some("out.txt"), "main", true)));
        let off = strings(&["--no-changelog"]);
        assert_eq!(parse(&off), Ok(flags(None, "base", false)));
        assert!(parse(&strings(&["--nope"])).is_err());
        assert!(parse(&strings(&["--paths-out"])).is_err());
    }

    #[test]
    fn sdk_paths_are_relative_to_the_repository_root() {
        let cfg = sdk_config::parse(
            "[[sdk]]\nspec = \"openapi.yaml\"\nlang = \"python\"\nout = \"sdk/py\"\npackage = \"x\"\n",
        )
        .unwrap();
        let paths = sdk_paths(Path::new("/repo"), Path::new("/repo/crates/api"), &cfg);
        assert_eq!(paths, ["crates/api/sdk/py"]);
    }
}
