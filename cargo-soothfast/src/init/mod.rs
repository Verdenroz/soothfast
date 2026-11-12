//! `cargo soothfast init`: wire a repo into soothfast. The CI workflow is
//! derived from the cargo workspace at the root and the other-language
//! projects below it, so `init --check` fails once the repo gains a bench
//! target, a project or docs markers the workflow does not run.

use std::path::Path;

use crate::invoke;

pub(crate) mod bot;
pub mod ci;
mod detect;
pub(crate) mod ecosystem;
mod labels;
mod lookup;
mod report;
mod rust;
mod scaffold;
mod target;
mod textdiff;
mod workflow;

#[cfg(test)]
mod no_cargo_tests;
#[cfg(test)]
mod test_support;

use ci::Ci;
use ecosystem::{Ecosystem, Settings, normalize_dir};
use labels::{create_labels, label_exit};
pub(crate) use report::{Drift, drift};
use report::{report, skips_workflow};
pub(crate) use rust::benched;
pub use rust::{RustPlan, has_markers, rust_plan};
use scaffold::{Kind, scaffold};
use target::target;
pub use target::{ci, manifest, suites};
use workflow::WORKFLOW_PATH;

/// The toolchain versions and soothfast ref init's flags pin, over the ones an existing
/// workflow already carries.
#[derive(Default)]
struct Pins {
    java: Option<String>,
    node: Option<String>,
    /// The soothfast ref the workflow uses, over this release's tag.
    action: Option<String>,
}

pub fn run(args: &[String]) -> i32 {
    run_at(&invoke::project_root(), args)
}

fn run_at(root: &Path, args: &[String]) -> i32 {
    let mut check = false;
    let mut force = false;
    let mut bot = false;
    let mut no_bot = false;
    let mut labels = false;
    let mut no_labels = false;
    let mut bench: Vec<String> = Vec::new();
    let mut flags = Settings::default();
    let mut dir: Option<String> = None;
    let mut ci = Ci::default();
    let mut tools = Pins::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--help" | "-h" => {
                println!("{}", init_usage());
                return 0;
            }
            "--check" => check = true,
            "--force" => force = true,
            "--bot" => bot = true,
            "--no-bot" => no_bot = true,
            "--labels" => labels = true,
            "--no-labels" => no_labels = true,
            "-p" | "--package" => match it.next() {
                Some(p) => bench.push(p.clone()),
                None => return err("-p needs a package to add a bench target to"),
            },
            "--ecosystem" => match it.next().map(|e| Ecosystem::parse(e)) {
                Some(Ok(e)) => flags.ecosystem = Some(e),
                Some(Err(e)) => return err(&e),
                None => return err("--ecosystem needs rust, gradle, maven, python or node"),
            },
            "--runner-labels" => match it.next() {
                Some(labels) => ci.labels = Some(labels.split(',').map(String::from).collect()),
                None => return err("--runner-labels needs comma-separated runner labels"),
            },
            "--no-test" => ci.test = Some(false),
            "--dir" => match it.next().map(|d| normalize_dir(d)) {
                Some(Ok(d)) => dir = Some(d),
                Some(Err(e)) => return err(&e),
                None => return err("--dir needs a directory"),
            },
            "--pin" => match it.next() {
                Some(r)
                    if r.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c)) =>
                {
                    tools.action = Some(r.clone())
                }
                _ => return err("--pin needs a commit, tag or branch of soothfast"),
            },
            "--java" | "--node" => {
                let Some(version) = it.next().cloned() else {
                    return err(&format!("{a} needs a version"));
                };
                match a.as_str() {
                    "--java" => tools.java = Some(version),
                    _ => tools.node = Some(version),
                }
            }
            "--bench-cmd" | "--bench-out" | "--from" => {
                let Some(value) = it.next().cloned() else {
                    return err(&format!("{a} needs a value"));
                };
                match a.as_str() {
                    "--bench-cmd" => flags.bench_cmd = Some(value),
                    "--bench-out" => flags.bench_out = Some(value),
                    _ => flags.from = Some(value),
                }
            }
            other => return err(&format!("unknown init arg {other:?}")),
        }
    }
    if bot && no_bot {
        return err("--bot and --no-bot conflict");
    }
    if labels && no_bot {
        return err("--labels needs the bot, which --no-bot turns off");
    }
    let bot = !no_bot;
    if labels && no_labels {
        return err("--labels and --no-labels conflict");
    }
    let implied = bot && !labels && !no_labels;
    let labels = labels || implied;
    let (mut ws, notes) = match target(root, flags, dir, ci, tools, &bench, check, bot, false) {
        Ok(t) => t,
        Err(e) => return err(&e),
    };
    ws.labels = labels;
    for note in &notes {
        println!("{note}");
    }
    let callers = other_callers(root);
    for caller in &callers {
        println!(
            "warn     {caller} calls soothfast; its jobs run twice until it or {WORKFLOW_PATH} is removed"
        );
    }
    let duplicate_only = ws.projects.is_empty() && ws.ci.is_default();
    let skip_workflow = skips_workflow(root, &callers, duplicate_only, force);
    if skip_workflow {
        println!(
            "init: {WORKFLOW_PATH} not written, since it would only duplicate the jobs above; --force writes it"
        );
    }
    let files: Vec<_> = scaffold(&ws)
        .into_iter()
        .filter(|f| !skip_workflow || f.kind == Kind::Bot)
        .collect();
    if files.is_empty() {
        return 0;
    }
    let rc = report(root, &files, check, force);
    if check || rc != 0 {
        return rc;
    }
    println!("init: done. Next: {}", next_step(ws.rust));
    if bot {
        println!(
            "init: the bot needs the App installed once (https://github.com/apps/soothfast-bot); the soothfast-bot environment is created on the first run. --no-bot skips both."
        );
    }
    if labels {
        return label_exit(create_labels(root), implied);
    }
    0
}

/// The workflow files besides the one init writes that call soothfast, as
/// repo-relative paths.
fn other_callers(root: &Path) -> Vec<String> {
    let dir = root.join(".github/workflows");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let calls = |text: &str| {
        text.lines().any(|l| {
            let uses = l.trim().trim_start_matches("- ").strip_prefix("uses:");
            let target = uses.map(str::trim);
            target.is_some_and(|t| {
                t.strip_prefix("Verdenroz/soothfast")
                    .is_some_and(|rest| rest.starts_with(['@', '/']))
            })
        })
    };
    let mut found: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
        .filter(|p| p.file_name() != Path::new(WORKFLOW_PATH).file_name())
        .filter(|p| std::fs::read_to_string(p).is_ok_and(|t| calls(&t)))
        .filter_map(|p| Some(format!(".github/workflows/{}", p.file_name()?.to_str()?)))
        .collect();
    found.sort();
    found
}

/// The `init` entry of the CLI usage text.
fn init_usage() -> String {
    let lines = crate::USAGE
        .lines()
        .skip_while(|l| !l.starts_with("  init "));
    let rest = lines.skip(1).take_while(|l| l.starts_with("    "));
    let first = crate::USAGE.lines().find(|l| l.starts_with("  init "));
    first.into_iter().chain(rest).collect::<Vec<_>>().join("\n")
}

/// What to do after `init` before the first pull request gates anything.
fn next_step(rust: bool) -> &'static str {
    if rust {
        "annotate code with #[soothfast::bench] and push."
    } else {
        "check that the bench command writes its result file, then push."
    }
}

fn err(msg: &str) -> i32 {
    eprintln!("init: {msg}");
    2
}

#[cfg(test)]
mod tests {
    use super::bot::BotLabels;
    use super::ecosystem::{parse_projects_tolerant, parse_suites};
    use super::test_support::scratch;
    use super::*;

    const SHARED_TOML: &str = r##"[site]
name = "x"

[site.theme]
primary = "#336699"

[[site.nav]]
title = "Guide"
pages = ["index.md"]

[docs]
paths = ["README.md"]
package = "x"
coverage = 80

[[project]]
dir = "web"
ecosystem = "node"
future_key = "ignored"

[test]
ci = false

[gate]
codegen-units = 4

[changelog]
features = "a"

[package.x]
features = "all"

[package.x.gate]
features = "bench"
target = "perf"

[package.x.docs]
paths = ["book"]
coverage = 90

[[spec]]
path = "api.yaml"

[bot]
labels = true
merge_labels = ["ship"]

[[proto]]
proto = "wire.proto"
message = "Wire"
source = "src/wire.rs"
struct = "Wire"

[ci]
skip = ["sdk"]

[[sdk]]
spec = "api.yaml"
out = "sdk"
package = "x"

[[test.suite]]
label = "e2e"
project = "web"
cmd = "npm test"
"##;

    #[test]
    fn every_reader_ignores_the_tables_it_does_not_own() {
        assert_eq!(
            soothfast_site::config::parse(SHARED_TOML).unwrap().name,
            "x"
        );
        assert_eq!(
            crate::config::docs::parse(SHARED_TOML).unwrap().package,
            Some(vec!["x".into()])
        );
        assert_eq!(Ci::parse(SHARED_TOML).unwrap().test, Some(false));
        assert_eq!(parse_projects_tolerant(SHARED_TOML).unwrap().len(), 1);
        assert_eq!(parse_suites(SHARED_TOML).unwrap().len(), 1);
        assert_eq!(
            crate::config::gate::parse(SHARED_TOML)
                .unwrap()
                .codegen_units
                .as_deref(),
            Some("4")
        );
        assert!(crate::config::changelog::parse(SHARED_TOML).is_ok());
        assert_eq!(BotLabels::parse(SHARED_TOML).unwrap().merge, ["ship"]);
        assert_eq!(
            crate::spec_config::parse(SHARED_TOML)
                .unwrap()
                .entries
                .len(),
            1
        );
        assert_eq!(
            crate::sdk_config::parse(SHARED_TOML).unwrap().entries.len(),
            1
        );
        assert!(crate::test::config::parse(SHARED_TOML).is_ok());
        assert_eq!(
            crate::config::docs::parse(SHARED_TOML).unwrap().coverage,
            Some(80)
        );
        assert_eq!(crate::config::ci::parse(SHARED_TOML).unwrap().skip, ["sdk"]);
        assert_eq!(
            crate::spec_config::parse(SHARED_TOML).unwrap().protos.len(),
            1
        );
        assert_eq!(
            crate::config::package::parse(SHARED_TOML).unwrap()["x"]
                .base
                .features
                .as_deref(),
            Some("all")
        );
    }

    #[test]
    fn the_bot_flags_conflict_with_each_other_and_with_labels() {
        assert_eq!(run(&["--bot".into(), "--no-bot".into()]), 2);
        assert_eq!(run(&["--labels".into(), "--no-bot".into()]), 2);
    }

    #[test]
    fn labels_and_no_labels_conflict() {
        assert_eq!(run(&["--labels".into(), "--no-labels".into()]), 2);
    }

    #[test]
    fn other_workflows_that_call_soothfast_are_found() {
        let root = scratch("callers");
        let dir = root.join(".github/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        write(
            "ci.yml",
            "    steps:\n      - uses: Verdenroz/soothfast@abc # v1\n",
        );
        write(
            "reuse.yaml",
            "    uses: Verdenroz/soothfast/.github/workflows/run.yml@abc\n",
        );
        write("soothfast.yml", "    uses: Verdenroz/soothfast@abc\n");
        write(
            "other.yml",
            "      - uses: actions/checkout@abc\n# Verdenroz/soothfast@x\n",
        );
        assert_eq!(
            other_callers(&root),
            [".github/workflows/ci.yml", ".github/workflows/reuse.yaml"]
        );
        assert!(other_callers(&scratch("none")).is_empty());
    }

    #[test]
    fn the_init_usage_lists_init_alone() {
        let usage = init_usage();
        assert!(usage.starts_with("  init ") && usage.contains("--dir D"));
        assert!(usage.contains("--runner-labels") && !usage.contains("import "));
    }
}
