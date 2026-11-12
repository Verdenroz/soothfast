//! `cargo soothfast ci <kind>`: one CI job as one command, the same in a
//! workflow and on a developer's machine.
//!
//! A kind is a function over the run's [`CiEnv`] and its own arguments, and
//! returns the job's exit code. Adding one is a file with that function, a
//! `mod` line and its entry in [`KINDS`].

mod build;
mod capture;
pub mod checks;
mod coverage;
mod docs;
mod each;
mod env;
mod gate;
mod import;
mod proto;
mod record;
mod regen;
mod site;
mod spec;
mod test;
mod verdict;

use env::{CiEnv, Event, split_flags};

/// A job `ci` can run. `run` is `None` while the name is reserved and the job
/// still lives in the workflow.
struct Kind {
    name: &'static str,
    run: Option<fn(&CiEnv, &[String]) -> i32>,
    applies: fn(&CiEnv) -> bool,
}

/// Every kind, in the order bare `ci` runs them.
const KINDS: &[Kind] = &[
    Kind {
        name: "test",
        run: Some(test::run),
        applies: |_| true,
    },
    Kind {
        name: "build",
        run: Some(build::run),
        applies: |_| false,
    },
    Kind {
        name: "gate",
        run: Some(gate::run),
        applies: in_a_run,
    },
    Kind {
        name: "gate-import",
        run: Some(import::run),
        applies: in_a_run,
    },
    Kind {
        name: "docs",
        run: Some(docs::run),
        applies: in_a_run,
    },
    Kind {
        name: "spec",
        run: Some(spec::run),
        applies: in_a_run,
    },
    Kind {
        name: "proto",
        run: Some(proto::run),
        applies: in_a_run,
    },
    Kind {
        name: "coverage",
        run: Some(coverage::run),
        applies: in_a_run,
    },
    Kind {
        name: "capture",
        run: Some(capture::run),
        applies: in_a_run,
    },
    Kind {
        name: "site",
        run: Some(site::run),
        applies: in_a_run,
    },
    Kind {
        name: "verdict",
        run: Some(verdict::run),
        applies: |e| e.event == Event::PullRequest,
    },
    Kind {
        name: "regen",
        run: Some(regen::run),
        applies: |e| e.event == Event::Push && e.default_branch,
    },
];

/// Whether the run came from a pull request or a push. Measuring, building
/// and docs jobs belong to those; a bare local `ci` runs only what is cheap.
fn in_a_run(env: &CiEnv) -> bool {
    env.event != env::Event::Other
}

/// Entry point for `cargo soothfast ci`.
pub fn run(args: &[String]) -> i32 {
    let (flags, rest) = match split_flags(args) {
        Ok(split) => split,
        Err(e) => return crate::arg_err(&e),
    };
    dispatch(KINDS, &CiEnv::from_process(&flags), &rest)
}

fn dispatch(kinds: &[Kind], env: &CiEnv, args: &[String]) -> i32 {
    match args.split_first() {
        Some((name, rest)) if !name.starts_with('-') => run_kind(kinds, env, name, rest),
        Some((arg, _)) => crate::arg_err(&format!("unknown ci argument {arg:?}")),
        None => run_all(kinds, env),
    }
}

fn run_kind(kinds: &[Kind], env: &CiEnv, name: &str, args: &[String]) -> i32 {
    let Some(kind) = kinds.iter().find(|k| k.name == name) else {
        return crate::arg_err(&format!("unknown ci kind {name:?}"));
    };
    match kind.run {
        Some(run) => run(env, args),
        None => {
            eprintln!("soothfast: ci {name} is not implemented");
            2
        }
    }
}

fn run_all(kinds: &[Kind], env: &CiEnv) -> i32 {
    for kind in kinds.iter().filter(|k| (k.applies)(env)) {
        let Some(run) = kind.run else { continue };
        let code = run(env, &[]);
        if code != 0 {
            return code;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use env::{Event, Flags};
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    thread_local! {
        static RAN: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }

    fn env() -> CiEnv {
        CiEnv::parse(&BTreeMap::new(), None, &Flags::default())
    }

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn table() -> [Kind; 4] {
        [
            Kind {
                name: "one",
                run: Some(|_, args| {
                    RAN.with_borrow_mut(|r| r.push("one"));
                    args.len() as i32
                }),
                applies: |_| true,
            },
            Kind {
                name: "pushes",
                run: Some(|_, _| {
                    RAN.with_borrow_mut(|r| r.push("pushes"));
                    0
                }),
                applies: |e| e.event == Event::Push,
            },
            Kind {
                name: "fails",
                run: Some(|_, _| {
                    RAN.with_borrow_mut(|r| r.push("fails"));
                    4
                }),
                applies: |_| true,
            },
            Kind {
                name: "reserved",
                run: None,
                applies: |_| true,
            },
        ]
    }

    #[test]
    fn unknown_kind_is_a_usage_error() {
        assert_eq!(dispatch(&table(), &env(), &argv(&["nope"])), 2);
        assert_eq!(dispatch(&table(), &env(), &argv(&["--nope"])), 2);
    }

    #[test]
    fn reserved_kind_is_not_implemented() {
        assert_eq!(dispatch(&table(), &env(), &argv(&["reserved"])), 2);
    }

    #[test]
    fn a_kind_gets_its_own_arguments_and_its_exit_code_is_the_result() {
        assert_eq!(dispatch(&table(), &env(), &argv(&["fails", "x"])), 4);
        assert_eq!(dispatch(&table(), &env(), &argv(&["one", "a", "b"])), 2);
    }

    #[test]
    fn bare_ci_runs_applicable_kinds_in_order_and_stops_at_a_failure() {
        RAN.with_borrow_mut(Vec::clear);
        assert_eq!(dispatch(&table(), &env(), &[]), 4);
        assert_eq!(RAN.with_borrow(Clone::clone), ["one", "fails"]);

        RAN.with_borrow_mut(Vec::clear);
        let push = CiEnv {
            event: Event::Push,
            ..env()
        };
        assert_eq!(dispatch(&table(), &push, &[]), 4);
        assert_eq!(RAN.with_borrow(Clone::clone), ["one", "pushes", "fails"]);
    }

    #[test]
    fn every_reserved_name_is_registered() {
        let names: Vec<&str> = KINDS.iter().map(|k| k.name).collect();
        assert_eq!(
            names,
            [
                "test",
                "build",
                "gate",
                "gate-import",
                "docs",
                "spec",
                "proto",
                "coverage",
                "capture",
                "site",
                "verdict",
                "regen"
            ]
        );
    }
}
