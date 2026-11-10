//! `ci gate`: the perf gate job. A pull request gates each package against
//! its base, and a push to the default branch records HEAD against its parent
//! so later pull requests find a reference run.

use std::process::Command;

use super::env::{CiEnv, Event};
use crate::invoke::Shard;

const NO_PARENT: &str = "no-parent";

#[derive(Debug, Default, PartialEq)]
struct Options {
    packages: Vec<String>,
    shard: Option<Shard>,
    save: Option<String>,
}

/// What the run does with each package.
#[derive(Debug, PartialEq)]
enum Plan {
    /// Gate against `against`, or against what `gate` defaults to.
    Compare {
        against: Option<String>,
    },
    /// Gate HEAD against its parent to store the run, or mark the packages
    /// unmeasured when there is no parent.
    Record {
        parent: bool,
    },
    Skip,
}

/// Entry point for `ci gate`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let opts = match parse(args) {
        Ok(opts) => opts,
        Err(e) => return crate::arg_err(&e),
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    let packages = match packages(&opts) {
        Ok(packages) => packages,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    let invoke = |args: &[String], record: bool| {
        let mut cmd = Command::new(&exe);
        cmd.args(args);
        if record {
            cmd.env("SOOTHFAST_RECORD", "1");
        }
        cmd.status().map_or(2, |s| s.code().unwrap_or(1))
    };
    match plan(env, has_parent()) {
        Plan::Skip => {
            println!("soothfast: ci gate has nothing to do for this event");
            0
        }
        Plan::Record { parent } => record(&opts, &packages, parent, invoke),
        Plan::Compare { against } => compare(&opts, &packages, against.as_deref(), invoke),
    }
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
            "--shard" => opts.shard = Some(Shard::parse(&value()?)?),
            "--save-baseline" => opts.save = Some(value()?),
            _ => return Err(format!("unknown ci gate argument {a:?}")),
        }
    }
    Ok(opts)
}

fn packages(opts: &Options) -> Result<Vec<String>, String> {
    if !opts.packages.is_empty() {
        return Ok(opts.packages.clone());
    }
    let meta = crate::workspace::metadata(None, true).map_err(|e| e.to_string())?;
    let found = crate::init::benched(&meta);
    if found.is_empty() {
        return Err("no package has a bench target named soothfast; pass -p".into());
    }
    Ok(found)
}

fn has_parent() -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "HEAD^"])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn plan(env: &CiEnv, has_parent: bool) -> Plan {
    match env.event {
        Event::Push if env.default_branch => Plan::Record { parent: has_parent },
        Event::Push => Plan::Skip,
        Event::PullRequest | Event::Other => Plan::Compare {
            against: env.base.clone(),
        },
    }
}

fn gate_args(
    pkg: &str,
    against: Option<&str>,
    save: Option<&str>,
    shard: Option<Shard>,
) -> Vec<String> {
    let mut args = vec!["gate".into(), "-p".into(), pkg.into()];
    if let Some(against) = against {
        args.extend(["--against-ref".into(), against.into()]);
    }
    if let Some(save) = save {
        args.extend(["--save-baseline".into(), save.into()]);
    }
    if let Some(shard) = shard {
        args.extend(["--shard".into(), shard.to_string()]);
    }
    args
}

fn mark_args(pkg: &str, into: &str) -> Vec<String> {
    let detail = format!("HEAD has no parent to gate {pkg} against");
    let mut args = vec!["baseline".into(), "mark".into(), "--into".into()];
    args.extend([into.into(), "-p".into(), pkg.into()]);
    args.extend([
        "--reason".into(),
        NO_PARENT.into(),
        "--detail".into(),
        detail,
    ]);
    args
}

fn compare(
    opts: &Options,
    packages: &[String],
    against: Option<&str>,
    invoke: impl Fn(&[String], bool) -> i32,
) -> i32 {
    packages
        .iter()
        .map(|pkg| {
            let args = gate_args(pkg, against, opts.save.as_deref(), opts.shard);
            invoke(&args, false)
        })
        .max()
        .unwrap_or(0)
}

/// Never fails: the commit is already merged, and what runs after still
/// must.
fn record(
    opts: &Options,
    packages: &[String],
    parent: bool,
    invoke: impl Fn(&[String], bool) -> i32,
) -> i32 {
    if !parent {
        eprintln!("soothfast: HEAD has no parent to gate against; no reference run recorded");
    }
    for pkg in packages {
        let (args, record) = match (parent, opts.save.as_deref()) {
            (true, save) => (gate_args(pkg, Some("HEAD^"), save, None), true),
            (false, Some(into)) => (mark_args(pkg, into), false),
            (false, None) => continue,
        };
        let code = invoke(&args, record);
        if code != 0 {
            eprintln!("soothfast: {} for {pkg} exited {code}", args[..2].join(" "));
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::env::Flags;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    fn env(event: Event, base: Option<&str>, default_branch: bool) -> CiEnv {
        let flags = Flags {
            event: Some(event),
            base: base.map(String::from),
            results: None,
        };
        CiEnv {
            default_branch,
            ..CiEnv::parse(&BTreeMap::new(), None, &flags)
        }
    }

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn shard(index: u64, count: u64) -> Option<Shard> {
        Some(Shard { index, count })
    }

    #[test]
    fn a_pull_request_gates_against_its_base() {
        let plan = plan(&env(Event::PullRequest, Some("b4se"), false), true);
        assert_eq!(
            plan,
            Plan::Compare {
                against: Some("b4se".into())
            }
        );
        assert_eq!(
            gate_args("core", Some("b4se"), Some("main"), shard(1, 2)),
            [
                "gate",
                "-p",
                "core",
                "--against-ref",
                "b4se",
                "--save-baseline",
                "main",
                "--shard",
                "1/2"
            ]
        );
    }

    #[test]
    fn a_local_run_gates_against_its_base_or_leaves_it_to_gate() {
        let with = plan(&env(Event::Other, Some("origin/main"), false), true);
        assert_eq!(
            with,
            Plan::Compare {
                against: Some("origin/main".into())
            }
        );
        let without = plan(&env(Event::Other, None, false), true);
        assert_eq!(without, Plan::Compare { against: None });
        assert_eq!(gate_args("core", None, None, None), ["gate", "-p", "core"]);
    }

    #[test]
    fn a_default_branch_push_records_and_other_pushes_do_nothing() {
        let push = |default| plan(&env(Event::Push, Some("pr3v"), default), true);
        assert_eq!(push(true), Plan::Record { parent: true });
        assert_eq!(push(false), Plan::Skip);
        let root = plan(&env(Event::Push, None, true), false);
        assert_eq!(root, Plan::Record { parent: false });
    }

    fn recorded(parent: bool, save: Option<&str>) -> (i32, Vec<(Vec<String>, bool)>) {
        let calls = RefCell::new(Vec::new());
        let opts = Options {
            save: save.map(String::from),
            shard: shard(1, 2),
            ..Options::default()
        };
        let invoke = |args: &[String], record: bool| {
            calls.borrow_mut().push((args.to_vec(), record));
            3
        };
        let code = record(&opts, &argv(&["a", "b"]), parent, invoke);
        (code, calls.into_inner())
    }

    #[test]
    fn a_push_gates_head_against_its_parent_with_recording_on() {
        let (code, calls) = recorded(true, Some("main"));
        assert_eq!(code, 0);
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].0,
            [
                "gate",
                "-p",
                "a",
                "--against-ref",
                "HEAD^",
                "--save-baseline",
                "main"
            ]
        );
        assert!(calls.iter().all(|(_, record)| *record));
    }

    #[test]
    fn a_push_without_a_parent_marks_each_package_when_saving() {
        let (code, calls) = recorded(false, Some("main"));
        assert_eq!(code, 0);
        assert_eq!(
            calls[1].0,
            [
                "baseline",
                "mark",
                "--into",
                "main",
                "-p",
                "b",
                "--reason",
                "no-parent",
                "--detail",
                "HEAD has no parent to gate b against"
            ]
        );
        assert!(recorded(false, None).1.is_empty());
    }

    #[test]
    fn arguments_parse_and_reject_the_unknown() {
        let opts = parse(&argv(&[
            "-p",
            "a",
            "-p",
            "b",
            "--shard",
            "2/3",
            "--save-baseline",
            "n",
        ]))
        .unwrap();
        assert_eq!(opts.packages, ["a", "b"]);
        assert_eq!(opts.shard, shard(2, 3));
        assert_eq!(opts.save.as_deref(), Some("n"));
        assert!(parse(&argv(&["--shard", "0/2"])).is_err());
        assert!(parse(&argv(&["-p"])).is_err());
        assert!(parse(&argv(&["--nope"])).is_err());
    }
}
