//! `cargo soothfast prebuild`: build into the stores what a later job reads,
//! and nothing else, so that job compiles nothing.
//!
//! What a job reads is named by its inputs: the bench packages and the
//! merge-base to gate against (`-p`, `--against-ref`), the package the docs
//! check documents (`--docs`), and the packages `spec` commands run on
//! (`--spec PKG[:TARGET]`). Each is looked up in the store first and built
//! only on a miss. The merge-base side is also left alone when the run cache
//! already holds a run for it, which is the gate's own test for not building
//! that side.
//!
//! `--only bench|routes|rustdoc` restricts the run to one lane, so the lanes
//! can run on separate machines: each fills a different part of the stores.
//! `--stage DIR` copies just the entries the run built or found, so the
//! machine hands on those and not everything it restored.

mod args;
mod build;
mod live;
mod select;
mod stage;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use crate::invoke;
use crate::{seed, workspace};

use args::{Args, parse};
use build::execute;
use live::{Live, Memo, Tree};
use select::{Action, Item, Lane, Outcome, Side, State, Step, Wanted, select, select_base};

pub fn run(args: &[String]) -> i32 {
    let Args { common, wanted } = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    let memo = Memo::default();
    let base_sha = wanted
        .against_ref
        .as_deref()
        .filter(|_| wanted.lanes.contains(&Lane::Bench))
        .and_then(|r| invoke::git(&["merge-base", "HEAD", r]).ok())
        .unwrap_or_default();
    let live = Live {
        common: &common,
        base: None,
        base_sha: base_sha.clone(),
        stage: wanted.stage.as_deref(),
        shards: wanted.shards,
        reuse_base: wanted.reuse_base,
        memo: &memo,
    };
    let steps = select(&wanted, &live);
    let outcomes = execute(&steps, &live);
    let mut report: Vec<(Step, Outcome)> = steps.into_iter().zip(outcomes).collect();

    let head_built = head_benches_built(&report);
    if !head_built {
        println!("prebuild: no merge-base side: HEAD's bench executables did not all build");
    }
    if head_built
        && let Some(refname) = wanted
            .against_ref
            .as_deref()
            .filter(|_| wanted.lanes.contains(&Lane::Bench))
    {
        match base_side(&live, &wanted, refname) {
            Ok(done) => report.extend(done),
            Err(e) => eprintln!("prebuild: no merge-base side: {e}"),
        }
    }
    finish(&report)
}

/// Whether no bench executable of HEAD failed to build. Only those stop the
/// merge-base side, since the gate has nothing to compare without them.
fn head_benches_built(report: &[(Step, Outcome)]) -> bool {
    report.iter().all(|(step, outcome)| {
        let head_bench = matches!(
            step.item,
            Item::Bench {
                side: Side::Head,
                ..
            }
        );
        !(head_bench && matches!(outcome, Outcome::Failed(_)))
    })
}

/// The merge-base's executables, built in its worktree the way the gate
/// builds them.
fn base_side(head: &Live, wanted: &Wanted, refname: &str) -> Result<Vec<(Step, Outcome)>, String> {
    let covered = |pkg: &String| Step {
        item: Item::Bench {
            pkg: pkg.clone(),
            side: Side::Base,
        },
        action: Action::Covered,
    };
    // The worktree is only made when some package needs the merge-base built.
    if wanted.packages.is_empty()
        || (wanted.reuse_base && wanted.packages.iter().all(|p| head.reference_stored(p)))
    {
        wanted.packages.iter().for_each(|p| head.stage_reference(p));
        return Ok(wanted
            .packages
            .iter()
            .map(|p| (covered(p), Outcome::Covered))
            .collect());
    }
    let target = invoke::worktree_target_dir().map_err(|e| e.to_string())?;
    let bench = head.common.target.as_deref().unwrap_or("soothfast");
    invoke::with_merge_base_worktree(refname, |wt| {
        let packages = wanted
            .packages
            .iter()
            .filter(|p| workspace::has_bench_target(p, bench, Some(wt)).unwrap_or(false))
            .cloned()
            .collect();
        invoke::sync_untracked_cargo_config(wt)?;
        invoke::sync_harness_versions(wt)?;
        seed::seed_worktree(wt, &target);
        let live = Live {
            common: head.common,
            base: Some(Tree {
                wt: wt.to_path_buf(),
                target,
            }),
            base_sha: head.base_sha.clone(),
            stage: head.stage,
            shards: head.shards,
            reuse_base: head.reuse_base,
            memo: head.memo,
        };
        let wanted = Wanted {
            packages,
            ..wanted.clone()
        };
        let steps = select_base(&wanted, &live);
        let outcomes = execute(&steps, &live);
        Ok(steps.into_iter().zip(outcomes).collect())
    })
}

fn described(item: &Item, outcome: &Outcome) -> String {
    match (item, outcome) {
        (Item::Rustdoc { pkg, .. }, Outcome::Covered) => {
            format!("rustdoc json {pkg} (no Rust binds)")
        }
        (item, _) => item.to_string(),
    }
}

/// Print what happened to each step and a total. A merge-base side that did
/// not build is no failure here: the gate reports it with its own output.
fn finish(report: &[(Step, Outcome)]) -> i32 {
    let mut counts = BTreeMap::new();
    let mut failed = false;
    for (step, outcome) in report {
        let label = match outcome {
            Outcome::Built => "built",
            Outcome::Stored => "stored",
            Outcome::Covered => "skipped",
            Outcome::Unstored => "unstored",
            Outcome::Failed(_) => "FAILED",
        };
        *counts.entry(label).or_insert(0) += 1;
        println!("prebuild: {label:<8} {}", described(&step.item, outcome));
        if let Outcome::Failed(why) = outcome {
            eprintln!("prebuild: {why}");
            failed |= !matches!(
                step.item,
                Item::Bench {
                    side: Side::Base,
                    ..
                }
            );
        }
    }
    let total: Vec<String> = counts.iter().map(|(l, n)| format!("{n} {l}")).collect();
    println!(
        "prebuild: {}",
        if total.is_empty() {
            "nothing to build".to_string()
        } else {
            total.join(", ")
        }
    );
    i32::from(failed)
}
