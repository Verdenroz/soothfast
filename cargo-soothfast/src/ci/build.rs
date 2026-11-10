//! `ci build`: the build-once job, which prebuilds into the stores what later
//! jobs read: bench executables, routes executables or rustdoc JSON.

use std::path::PathBuf;

use super::env::{CiEnv, Event};
use crate::init::RustPlan;

/// What `ci build` was asked for.
#[derive(Debug, PartialEq)]
struct Job {
    /// A lane name, handed to `prebuild --only` as given so it decides what
    /// is valid.
    lane: String,
    packages: Vec<String>,
    shards: u64,
    stage: Option<PathBuf>,
    plan_out: Option<PathBuf>,
    listing_out: Option<PathBuf>,
}

/// What the run's event decides about every `prebuild` call.
#[derive(Debug, PartialEq)]
struct Shape {
    shards: u64,
    against: Option<String>,
}

fn parse(args: &[String]) -> Result<Job, String> {
    let mut job = Job {
        lane: String::new(),
        packages: Vec::new(),
        shards: 1,
        stage: None,
        plan_out: None,
        listing_out: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "-p" | "--package" => job.packages.push(value()?),
            "--shards" => match value()?.parse() {
                Ok(n) if n >= 1 => job.shards = n,
                _ => return Err("--shards wants a count of at least 1".into()),
            },
            "--stage" => job.stage = Some(value()?.into()),
            "--plan-out" => job.plan_out = Some(value()?.into()),
            "--listing-out" => job.listing_out = Some(value()?.into()),
            lane if !lane.starts_with('-') && job.lane.is_empty() => job.lane = lane.into(),
            other => return Err(format!("unknown ci build argument {other:?}")),
        }
    }
    if job.lane.is_empty() {
        return Err("ci build needs a lane".into());
    }
    Ok(job)
}

/// Only a pull request's gate is split into legs, so only a pull request
/// builds for more than one.
fn shape(env: &CiEnv, asked: u64, has_parent: impl Fn() -> bool) -> Shape {
    let against = match env.event {
        Event::PullRequest => env.base.clone(),
        Event::Push if env.default_branch && has_parent() => Some("HEAD^".into()),
        _ => None,
    };
    Shape {
        shards: if env.event == Event::PullRequest {
            asked
        } else {
            1
        },
        against,
    }
}

/// The packages a lane applies to when none are named.
fn default_packages(lane: &str, plan: &RustPlan) -> Vec<String> {
    match lane {
        "bench" => plan.packages.clone(),
        "rustdoc" => plan.docs_packages.clone(),
        _ => Vec::new(),
    }
}

fn package_flag(lane: &str) -> &'static str {
    match lane {
        "rustdoc" => "--docs",
        "routes" => "--spec",
        _ => "-p",
    }
}

/// One `prebuild` call per package, so each resolves its own features.
fn invocations(
    job: &Job,
    packages: &[String],
    shape: &Shape,
    docs_features: impl Fn(&str) -> Result<Option<String>, String>,
) -> Result<Vec<Vec<String>>, String> {
    packages
        .iter()
        .map(|pkg| {
            let mut args = vec!["prebuild".into(), "--only".into(), job.lane.clone()];
            if let Some(stage) = &job.stage {
                args.extend(["--stage".into(), stage.display().to_string()]);
            }
            if shape.shards > 1 {
                args.extend(["--shards".into(), shape.shards.to_string()]);
            }
            args.extend([package_flag(&job.lane).into(), pkg.clone()]);
            if job.lane == "rustdoc"
                && let Some(features) = docs_features(pkg)?
            {
                args.extend(["--docs-features".into(), features]);
            }
            if let Some(against) = &shape.against {
                args.extend(["--against-ref".into(), against.clone()]);
            }
            Ok(args)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::env::Flags;
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

    fn job(lane: &str, stage: Option<&str>) -> Job {
        Job {
            lane: lane.into(),
            packages: Vec::new(),
            shards: 1,
            stage: stage.map(Into::into),
            plan_out: None,
            listing_out: None,
        }
    }

    fn no_features(_: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    #[test]
    fn parses_a_lane_and_its_flags() {
        let args = argv(&[
            "bench",
            "-p",
            "a",
            "--package",
            "b",
            "--shards",
            "3",
            "--stage",
            "store",
            "--plan-out",
            "plan.json",
            "--listing-out",
            "listing",
        ]);
        let parsed = parse(&args).unwrap();
        assert_eq!(parsed.lane, "bench");
        assert_eq!(parsed.packages, ["a", "b"]);
        assert_eq!(parsed.shards, 3);
        assert_eq!(parsed.stage, Some("store".into()));
        assert_eq!(parsed.plan_out, Some("plan.json".into()));
        assert_eq!(parsed.listing_out, Some("listing".into()));
    }

    #[test]
    fn a_lane_name_is_left_for_prebuild_to_judge() {
        assert_eq!(parse(&argv(&["whatever"])).unwrap().lane, "whatever");
    }

    #[test]
    fn bad_arguments_are_errors() {
        assert!(parse(&[]).is_err());
        assert!(parse(&argv(&["bench", "-p"])).is_err());
        assert!(parse(&argv(&["bench", "--shards", "0"])).is_err());
        assert!(parse(&argv(&["bench", "--shards", "x"])).is_err());
        assert!(parse(&argv(&["bench", "other"])).is_err());
        assert!(parse(&argv(&["bench", "--nope"])).is_err());
    }

    #[test]
    fn a_pull_request_shards_and_compares_with_its_base() {
        let shape = shape(&env(Event::PullRequest, Some("b4se"), false), 4, || true);
        assert_eq!(
            shape,
            Shape {
                shards: 4,
                against: Some("b4se".into())
            }
        );
    }

    #[test]
    fn a_push_builds_one_unsharded_lane() {
        let on_default = shape(&env(Event::Push, Some("old"), true), 4, || true);
        assert_eq!(
            on_default,
            Shape {
                shards: 1,
                against: Some("HEAD^".into())
            }
        );
        let orphan = shape(&env(Event::Push, Some("old"), true), 4, || false);
        assert_eq!(orphan.against, None);
        let elsewhere = shape(&env(Event::Push, Some("old"), false), 4, || true);
        assert_eq!(elsewhere.against, None);
    }

    #[test]
    fn a_local_run_has_nothing_to_compare_with() {
        let shape = shape(&env(Event::Other, Some("b4se"), true), 4, || true);
        assert_eq!(
            shape,
            Shape {
                shards: 1,
                against: None
            }
        );
    }

    #[test]
    fn each_lane_applies_to_its_own_packages() {
        let plan = RustPlan {
            packages: vec!["a".into(), "b".into()],
            docs_packages: vec!["d".into()],
        };
        assert_eq!(default_packages("bench", &plan), ["a", "b"]);
        assert_eq!(default_packages("rustdoc", &plan), ["d"]);
        assert!(default_packages("routes", &plan).is_empty());
    }

    #[test]
    fn bench_builds_one_package_per_call_against_the_base() {
        let shape = shape(&env(Event::PullRequest, Some("b4se"), false), 2, || true);
        let calls = invocations(
            &job("bench", Some("store")),
            &argv(&["a", "b"]),
            &shape,
            no_features,
        )
        .unwrap();
        let call = |pkg: &str| {
            argv(&[
                "prebuild",
                "--only",
                "bench",
                "--stage",
                "store",
                "--shards",
                "2",
                "-p",
                pkg,
                "--against-ref",
                "b4se",
            ])
        };
        assert_eq!(calls, [call("a"), call("b")]);
    }

    #[test]
    fn an_unsharded_build_passes_no_shard_count() {
        let shape = shape(&env(Event::Push, None, false), 4, || true);
        let calls = invocations(&job("bench", None), &argv(&["a"]), &shape, no_features).unwrap();
        assert_eq!(calls, [argv(&["prebuild", "--only", "bench", "-p", "a"])]);
    }

    #[test]
    fn rustdoc_names_docs_packages_and_their_features() {
        let shape = shape(&env(Event::Other, None, false), 1, || false);
        let features = |pkg: &str| Ok((pkg == "d").then(|| "full".to_string()));
        let calls = invocations(&job("rustdoc", None), &argv(&["d", "e"]), &shape, features);
        assert_eq!(
            calls.unwrap(),
            [
                argv(&[
                    "prebuild",
                    "--only",
                    "rustdoc",
                    "--docs",
                    "d",
                    "--docs-features",
                    "full"
                ]),
                argv(&["prebuild", "--only", "rustdoc", "--docs", "e"]),
            ]
        );
    }

    #[test]
    fn routes_names_spec_packages() {
        let shape = shape(&env(Event::Other, None, false), 1, || false);
        let calls = invocations(
            &job("routes", None),
            &argv(&["api:srv"]),
            &shape,
            no_features,
        );
        assert_eq!(
            calls.unwrap(),
            [argv(&["prebuild", "--only", "routes", "--spec", "api:srv"])]
        );
    }

    #[test]
    fn a_features_lookup_error_is_reported() {
        let shape = shape(&env(Event::Other, None, false), 1, || false);
        let failing = |_: &str| Err("bad table".to_string());
        let calls = invocations(&job("rustdoc", None), &argv(&["d"]), &shape, failing);
        assert_eq!(calls, Err("bad table".to_string()));
    }
}
