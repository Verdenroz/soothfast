//! `ci build`: the build-once job, which prebuilds into the stores what later
//! jobs read: bench executables, routes executables or rustdoc JSON.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use soothfast_registry::fnv1a;

use super::env::{CiEnv, Event};
use crate::config::{self, Group, Settings};
use crate::init;
use crate::{invoke, spec_config, workspace};

/// What `ci build` was asked for.
#[derive(Debug, PartialEq)]
struct Job {
    lane: String,
    /// The packages named, each with the flag that named it.
    named: Vec<(String, String)>,
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
        named: Vec::new(),
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
            "-p" | "--package" => job.named.push(("-p".into(), value()?)),
            "--docs" | "--spec" => job.named.push((a.clone(), value()?)),
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
    let flags: Vec<&str> = entries(&job.lane).map(|kind| kind.flag).collect();
    if flags.is_empty() {
        return Err(format!(
            "ci build wants bench, routes or rustdoc, got {:?}",
            job.lane
        ));
    }
    if let Some((flag, _)) = job.named.iter().find(|(f, _)| !flags.contains(&f.as_str())) {
        return Err(format!(
            "ci build {} takes {}, not {flag}",
            job.lane,
            flags.join(" and ")
        ));
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

/// Where a lane's packages come from when none are named.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Source {
    Gated,
    Docs,
    /// Packages with a `[[spec]]` entry.
    Spec,
    /// Packages that generate a spec from the code.
    Generating,
}

/// How `prebuild` takes a lane's packages and which defaults they resolve.
/// A lane with several entries builds for each. Naming packages for one entry
/// leaves the others with none.
struct LaneKind {
    name: &'static str,
    flag: &'static str,
    group: Group,
    source: Source,
}

const LANES: &[LaneKind] = &[
    LaneKind {
        name: "bench",
        flag: "-p",
        group: Group::Gate,
        source: Source::Gated,
    },
    LaneKind {
        name: "routes",
        flag: "--spec",
        group: Group::Spec,
        source: Source::Spec,
    },
    LaneKind {
        name: "rustdoc",
        flag: "--docs",
        group: Group::Docs,
        source: Source::Docs,
    },
    LaneKind {
        name: "rustdoc",
        flag: "--spec",
        group: Group::Spec,
        source: Source::Generating,
    },
];

fn entries(lane: &str) -> impl Iterator<Item = &'static LaneKind> {
    LANES.iter().filter(move |kind| kind.name == lane)
}

/// The `prebuild` calls for one entry's packages. Gated packages that resolve
/// different defaults build apart, in the order they first appear, since a
/// call takes one set of features. `prebuild` resolves the rest per package.
fn invocations(
    job: &Job,
    kind: &LaneKind,
    packages: &[String],
    shape: &Shape,
    resolve: impl Fn(&str, Group) -> Result<Settings, String>,
) -> Result<Vec<Vec<String>>, String> {
    let mut buckets: Vec<(Settings, Vec<&String>)> = Vec::new();
    for pkg in packages {
        let settings = match kind.group {
            Group::Gate => resolve(pkg, kind.group)?,
            _ => Settings::default(),
        };
        match buckets.iter_mut().find(|(s, _)| *s == settings) {
            Some((_, group)) => group.push(pkg),
            None => buckets.push((settings, vec![pkg])),
        }
    }
    Ok(buckets
        .into_iter()
        .map(|(_, group)| {
            let mut args = vec!["prebuild".into(), "--only".into(), job.lane.clone()];
            if let Some(stage) = &job.stage {
                args.extend(["--stage".into(), stage.display().to_string()]);
            }
            if shape.shards > 1 {
                args.extend(["--shards".into(), shape.shards.to_string()]);
            }
            for pkg in group {
                args.extend([kind.flag.into(), pkg.clone()]);
            }
            if let Some(against) = &shape.against {
                args.extend(["--against-ref".into(), against.clone()]);
            }
            args
        })
        .collect())
}

/// Entry point for `ci build`.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let job = match parse(args) {
        Ok(job) => job,
        Err(e) => return crate::arg_err(&e),
    };
    let has_parent = || invoke::git(&["rev-parse", "--verify", "--quiet", "HEAD^"]).is_ok();
    let shape = shape(env, job.shards, has_parent);
    let resolve = |pkg: &str, group| config::defaults(&[pkg.to_string()], group);
    let (packages, calls) = match plan(&job, &shape, load, resolve) {
        Ok(planned) => planned,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    if packages.is_empty() {
        println!("ci build: no packages for the {} lane", job.lane);
        return 0;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    let bins = invoke::project_root().join(".soothfast").join("bins");
    execute(
        &job,
        &packages,
        &calls,
        &bins,
        &mut |args| match Command::new(&exe).args(args).status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(e) => {
                eprintln!("soothfast: {e}");
                2
            }
        },
    )
}

/// The packages the job builds for and the `prebuild` calls that build them.
/// With no package named, each entry of the lane builds its own defaults.
fn plan(
    job: &Job,
    shape: &Shape,
    load: impl Fn(Source) -> Result<Vec<String>, String>,
    resolve: impl Fn(&str, Group) -> Result<Settings, String>,
) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
    let mut packages = Vec::new();
    let mut calls = Vec::new();
    for kind in entries(&job.lane) {
        let chosen = if job.named.is_empty() {
            load(kind.source)?
        } else {
            let named = job.named.iter().filter(|(flag, _)| flag == kind.flag);
            named.map(|(_, pkg)| pkg.clone()).collect()
        };
        calls.extend(invocations(job, kind, &chosen, shape, &resolve)?);
        packages.extend(chosen);
    }
    Ok((packages, calls))
}

fn load(source: Source) -> Result<Vec<String>, String> {
    let root = invoke::project_root();
    match source {
        Source::Gated => workspace::bench_crates("soothfast", Some(&root)),
        Source::Docs => Ok(init::rust_plan(&root)?.docs_packages),
        Source::Spec => Ok(spec_config::configured(&root)?.0),
        Source::Generating => Ok(spec_config::configured(&root)?.1),
    }
}

/// Runs every call, then lists the built items and the store's checksums as
/// the job asks. The first failing call's exit code is the result, after the
/// rest have run and the outputs are written, since a later job reads what
/// did build.
fn execute(
    job: &Job,
    packages: &[String],
    calls: &[Vec<String>],
    bins: &Path,
    exec: &mut dyn FnMut(&[String]) -> i32,
) -> i32 {
    let before = listing(bins);
    let mut code = 0;
    for call in calls {
        let status = exec(call);
        if code == 0 {
            code = status;
        }
    }
    if let Some(out) = &job.plan_out {
        list_items(out, packages, exec);
    }
    if let Some(out) = &job.listing_out
        && let Err(e) = write_listing(out, &before, &listing(bins))
    {
        eprintln!("soothfast: {}: {e}", out.display());
        code = if code == 0 { 2 } else { code };
    }
    code
}

/// A package whose items cannot be listed, because its build failed, is
/// skipped with a warning.
fn list_items(out: &Path, packages: &[String], exec: &mut dyn FnMut(&[String]) -> i32) {
    if let Some(dir) = out.parent() {
        let _ = fs::create_dir_all(dir);
    }
    for pkg in packages {
        let args = [
            "baseline".into(),
            "plan".into(),
            "--out".into(),
            out.display().to_string(),
            "-p".into(),
            pkg.clone(),
        ];
        if exec(&args) != 0 {
            eprintln!("soothfast: could not list {pkg}'s items for the docs check");
        }
    }
}

fn write_listing(out: &Path, before: &str, after: &str) -> std::io::Result<()> {
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(out, format!("before {before}\nafter {after}\n"))
}

/// A checksum over the names, relative to `dir`, and sizes of the files under
/// it, so two listings differ when a file is added, replaced or removed and
/// not when one is only read. It is the FNV-1a hash of the sorted
/// `name size` lines as 16 hex digits, `none` for a directory that does not
/// exist. Files ending in `.tmp` are not part of it.
fn listing(dir: &Path) -> String {
    if !dir.is_dir() {
        return "none".into();
    }
    let mut files = Vec::new();
    collect(dir, dir, &mut files);
    files.sort();
    let lines: String = files
        .iter()
        .map(|(name, size)| format!("{name} {size}\n"))
        .collect();
    format!("{:016x}", fnv1a(lines.as_bytes()))
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, u64)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if !path.to_string_lossy().ends_with(".tmp")
            && let (Ok(rel), Ok(meta)) = (path.strip_prefix(root), entry.metadata())
        {
            out.push((rel.display().to_string(), meta.len()));
        }
    }
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
            named: Vec::new(),
            shards: 1,
            stage: stage.map(Into::into),
            plan_out: None,
            listing_out: None,
        }
    }

    /// The calls for `packages` named under the lane's first flag.
    fn invocations(
        job: &Job,
        packages: &[String],
        shape: &Shape,
        resolve: impl Fn(&str, Group) -> Result<Settings, String>,
    ) -> Result<Vec<Vec<String>>, String> {
        let flag = entries(&job.lane).next().unwrap().flag;
        let job = Job {
            named: packages.iter().map(|p| (flag.into(), p.clone())).collect(),
            lane: job.lane.clone(),
            stage: job.stage.clone(),
            shards: job.shards,
            plan_out: None,
            listing_out: None,
        };
        let unused = |_| Err("named packages need no defaults".to_string());
        plan(&job, shape, unused, resolve).map(|(_, calls)| calls)
    }

    fn no_features(_: &str, _: Group) -> Result<Settings, String> {
        Ok(Settings::default())
    }

    fn features(
        of: &'static str,
        set: &'static str,
    ) -> impl Fn(&str, Group) -> Result<Settings, String> {
        move |pkg, _| {
            Ok(Settings {
                features: (pkg == of).then(|| set.to_string()),
                ..Settings::default()
            })
        }
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
        assert_eq!(
            parsed.named,
            [("-p".into(), "a".into()), ("-p".into(), "b".into())]
        );
        assert_eq!(parsed.shards, 3);
        assert_eq!(parsed.stage, Some("store".into()));
        assert_eq!(parsed.plan_out, Some("plan.json".into()));
        assert_eq!(parsed.listing_out, Some("listing".into()));
    }

    #[test]
    fn a_lane_takes_only_its_own_package_flags() {
        let named = |args: &[&str]| parse(&argv(args)).map(|job| job.named);
        assert_eq!(
            named(&["rustdoc", "--docs", "d", "--spec", "g"]).unwrap(),
            [("--docs".into(), "d".into()), ("--spec".into(), "g".into())]
        );
        assert!(named(&["rustdoc", "-p", "d"]).is_err());
        assert!(named(&["bench", "--docs", "d"]).is_err());
        assert!(named(&["whatever"]).is_err());
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

    fn configured(source: Source) -> Result<Vec<String>, String> {
        Ok(match source {
            Source::Gated => argv(&["a", "b"]),
            Source::Docs => argv(&["d"]),
            Source::Spec => argv(&["s", "g"]),
            Source::Generating => argv(&["g"]),
        })
    }

    #[test]
    fn each_lane_builds_its_own_packages_when_none_are_named() {
        let shape = shape(&env(Event::Other, None, false), 1, || false);
        let of = |lane| plan(&job(lane, None), &shape, configured, no_features).unwrap();
        assert_eq!(
            of("bench"),
            (
                argv(&["a", "b"]),
                vec![argv(&["prebuild", "--only", "bench", "-p", "a", "-p", "b"])]
            )
        );
        assert_eq!(
            of("routes").1,
            [argv(&[
                "prebuild", "--only", "routes", "--spec", "s", "--spec", "g"
            ])]
        );
        assert_eq!(
            of("rustdoc"),
            (
                argv(&["d", "g"]),
                vec![
                    argv(&["prebuild", "--only", "rustdoc", "--docs", "d"]),
                    argv(&["prebuild", "--only", "rustdoc", "--spec", "g"]),
                ]
            )
        );
    }

    #[test]
    fn naming_packages_for_one_entry_leaves_the_others_with_none() {
        let shape = shape(&env(Event::Other, None, false), 1, || false);
        let job = Job {
            named: vec![("--docs".into(), "x".into())],
            ..job("rustdoc", None)
        };
        let (packages, calls) = plan(&job, &shape, configured, no_features).unwrap();
        assert_eq!(packages, ["x"]);
        assert_eq!(
            calls,
            [argv(&["prebuild", "--only", "rustdoc", "--docs", "x"])]
        );
    }

    #[test]
    fn packages_that_resolve_alike_build_in_one_call() {
        let shape = shape(&env(Event::PullRequest, Some("b4se"), false), 2, || true);
        let calls = invocations(
            &job("bench", Some("store")),
            &argv(&["a", "b", "c"]),
            &shape,
            no_features,
        )
        .unwrap();
        assert_eq!(
            calls,
            [argv(&[
                "prebuild",
                "--only",
                "bench",
                "--stage",
                "store",
                "--shards",
                "2",
                "-p",
                "a",
                "-p",
                "b",
                "-p",
                "c",
                "--against-ref",
                "b4se",
            ])]
        );
    }

    #[test]
    fn packages_that_resolve_differently_build_apart_in_first_seen_order() {
        let shape = shape(&env(Event::Other, None, false), 1, || false);
        let calls = invocations(
            &job("bench", None),
            &argv(&["a", "b", "c"]),
            &shape,
            features("b", "full"),
        )
        .unwrap();
        assert_eq!(
            calls,
            [
                argv(&["prebuild", "--only", "bench", "-p", "a", "-p", "c"]),
                argv(&["prebuild", "--only", "bench", "-p", "b"]),
            ]
        );
    }

    #[test]
    fn only_gated_packages_resolve_defaults_here() {
        let shape = shape(&env(Event::Other, None, false), 1, || false);
        let seen = std::cell::RefCell::new(Vec::new());
        for lane in ["bench", "routes", "rustdoc"] {
            let resolve = |_: &str, group| {
                seen.borrow_mut().push(group);
                Ok(Settings::default())
            };
            invocations(&job(lane, None), &argv(&["p"]), &shape, resolve).unwrap();
        }
        assert_eq!(seen.into_inner(), [Group::Gate]);
    }

    #[test]
    fn an_unsharded_build_passes_no_shard_count() {
        let shape = shape(&env(Event::Push, None, false), 4, || true);
        let calls = invocations(&job("bench", None), &argv(&["a"]), &shape, no_features).unwrap();
        assert_eq!(calls, [argv(&["prebuild", "--only", "bench", "-p", "a"])]);
    }

    #[test]
    fn docs_packages_build_in_one_call_whatever_their_features() {
        let shape = shape(&env(Event::Other, None, false), 1, || false);
        let calls = invocations(
            &job("rustdoc", None),
            &argv(&["d", "e"]),
            &shape,
            features("d", "full"),
        );
        assert_eq!(
            calls.unwrap(),
            [argv(&[
                "prebuild", "--only", "rustdoc", "--docs", "d", "--docs", "e"
            ])]
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
        let failing = |_: &str, _| Err("bad table".to_string());
        let calls = invocations(&job("bench", None), &argv(&["a"]), &shape, failing);
        assert_eq!(calls, Err("bad table".to_string()));
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("soothfast-build-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn with_outputs(plan: Option<&Path>, listing: Option<&Path>) -> Job {
        Job {
            plan_out: plan.map(Into::into),
            listing_out: listing.map(Into::into),
            ..job("bench", None)
        }
    }

    #[test]
    fn every_call_runs_and_the_first_failure_is_the_result() {
        let calls = [argv(&["a"]), argv(&["b"]), argv(&["c"])];
        let mut seen = Vec::new();
        let codes = [0, 3, 4];
        let code = execute(
            &job("bench", None),
            &[],
            &calls,
            Path::new("missing"),
            &mut |args| {
                seen.push(args.to_vec());
                codes[seen.len() - 1]
            },
        );
        assert_eq!(code, 3);
        assert_eq!(seen, calls);
    }

    #[test]
    fn the_plan_loop_lists_each_package_and_skips_one_that_fails() {
        let dir = scratch("plan");
        let out = dir.join("nested").join("plan.json");
        let mut seen = Vec::new();
        let code = execute(
            &with_outputs(Some(&out), None),
            &argv(&["a", "b"]),
            &[],
            &dir.join("bins"),
            &mut |args| {
                seen.push(args.to_vec());
                (args.last().unwrap() == "a") as i32
            },
        );
        assert_eq!(code, 0);
        let plan = |pkg: &str| {
            argv(&[
                "baseline",
                "plan",
                "--out",
                &out.display().to_string(),
                "-p",
                pkg,
            ])
        };
        assert_eq!(seen, [plan("a"), plan("b")]);
        assert!(out.parent().unwrap().is_dir());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_build_still_writes_the_plan_and_listing() {
        let dir = scratch("failed");
        let (plan, listed) = (dir.join("plan.json"), dir.join("listing"));
        let mut seen = Vec::new();
        let code = execute(
            &with_outputs(Some(&plan), Some(&listed)),
            &argv(&["a"]),
            &[argv(&["prebuild"])],
            &dir.join("bins"),
            &mut |args| {
                seen.push(args[0].clone());
                7
            },
        );
        assert_eq!(code, 7);
        assert_eq!(seen, ["prebuild", "baseline"]);
        let text = fs::read_to_string(listed).unwrap();
        assert_eq!(text, "before none\nafter none\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_listing_file_holds_the_store_before_and_after_the_build() {
        let dir = scratch("both");
        let (bins, listed) = (dir.join("bins"), dir.join("listing"));
        fs::create_dir_all(&bins).unwrap();
        let before = listing(&bins);
        let code = execute(
            &with_outputs(None, Some(&listed)),
            &argv(&["a"]),
            &[argv(&["prebuild"])],
            &bins,
            &mut |_| {
                fs::write(bins.join("exe"), "code").unwrap();
                0
            },
        );
        assert_eq!(code, 0);
        let after = listing(&bins);
        assert_ne!(before, after);
        let text = fs::read_to_string(listed).unwrap();
        assert_eq!(text, format!("before {before}\nafter {after}\n"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unwritable_listing_is_an_error_exit() {
        let dir = scratch("unwritable");
        let blocker = dir.join("file");
        fs::write(&blocker, "").unwrap();
        let code = execute(
            &with_outputs(None, Some(&blocker.join("listing"))),
            &argv(&["a"]),
            &[],
            &dir,
            &mut |_| 0,
        );
        assert_eq!(code, 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_listing_is_stable_and_follows_names_and_sizes() {
        let dir = scratch("listing");
        assert_eq!(listing(&dir.join("absent")), "none");
        let empty = listing(&dir);
        fs::write(dir.join("a"), "12").unwrap();
        let one = listing(&dir);
        assert_ne!(one, empty);
        assert_eq!(listing(&dir), one);
        assert_eq!(one.len(), 16);

        fs::write(dir.join("a"), "123").unwrap();
        let resized = listing(&dir);
        assert_ne!(resized, one);
        fs::write(dir.join("a"), "xyz").unwrap();
        assert_eq!(listing(&dir), resized);

        fs::rename(dir.join("a"), dir.join("b")).unwrap();
        assert_ne!(listing(&dir), resized);

        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub").join("c"), "1").unwrap();
        let nested = listing(&dir);
        fs::write(dir.join("partial.tmp"), "1234").unwrap();
        assert_eq!(listing(&dir), nested);
        fs::remove_dir_all(&dir).unwrap();
    }
}
