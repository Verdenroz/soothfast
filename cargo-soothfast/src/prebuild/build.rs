//! Building the steps: each item's executable or rustdoc JSON into its store.

use std::path::{Path, PathBuf};

use super::live::{Head, Live, visibility};
use super::select::State;
use super::select::{Action, Item, Outcome, Side, Step};
use super::stage::stage_item;
use crate::invoke::{self, Build, CommonArgs, HostEnv};
use crate::itemreuse::{self, Bin};
use crate::{benchexe, headrun, linkmap};

/// Link the bench target a second time for its map, when the gate would read
/// one for this binary.
pub(super) fn link_map(
    common: &CommonArgs,
    exe: &Path,
    env: Option<&HostEnv>,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
) {
    let Some(env) = env else {
        return;
    };
    if !itemreuse::reads_maps(env, common) {
        return;
    }
    let digest = headrun::binary_digest(exe);
    let bin = Bin {
        exe,
        digest: digest.as_deref(),
        dir,
        target_dir,
        env: Some(env),
    };
    linkmap::extents("prebuild", common, &bin);
}

/// Builds what `step` names. The executable of a bench or routes step, which
/// staging then reads.
/// Document `pkg`. A failure of an aux crate warns and drops it.
fn build_rustdoc(
    pkg: &str,
    private: bool,
    features: Option<&str>,
    aux: bool,
) -> Result<Option<PathBuf>, String> {
    match invoke::rustdoc_json_in(pkg, None, features, visibility(private)) {
        Ok(_) => Ok(None),
        Err(e) if aux => {
            eprintln!("prebuild: cannot document workspace crate {pkg} ({e}); dropped");
            Ok(None)
        }
        Err(e) => Err(format!("{pkg}: {e}")),
    }
}

pub(super) fn build_item(step: &Step, live: &Live) -> Result<Option<PathBuf>, String> {
    let missing = |pkg: &str| format!("{pkg}: the executable did not build");
    match &step.item {
        Item::Bench { pkg, side } => {
            let (dir, target_dir) = live.tree(*side);
            let one = live.one(pkg, None);
            let exe = benchexe::executable(&one, dir, target_dir, Build::Measure)
                .ok_or_else(|| missing(pkg))?;
            let digest = headrun::binary_digest(&exe);
            let env = invoke::host_env(&exe, one.backend.as_deref()).ok();
            // Identical binaries need no map of the merge-base: the gate
            // measures the one pass.
            let same_as_head = *side == Side::Base
                && digest.is_some()
                && live.memo.heads.borrow().get(pkg).map(|h| &h.digest) == Some(&digest);
            if *side == Side::Head {
                live.memo.heads.borrow_mut().insert(
                    pkg.clone(),
                    Head {
                        digest,
                        env: env.clone(),
                    },
                );
            }
            if !same_as_head {
                link_map(&one, &exe, env.as_ref(), dir, target_dir);
            }
            Ok(Some(exe))
        }
        Item::Routes { pkg, target } => {
            benchexe::executable(&live.one(pkg, Some(target)), None, None, Build::Discover)
                .map(Some)
                .ok_or_else(|| missing(pkg))
        }
        Item::Rustdoc {
            pkg,
            private,
            features,
            aux,
        } => build_rustdoc(pkg, *private, features.as_deref(), *aux),
    }
}

/// Run `steps`, building the benches of one side together first so cargo
/// shares their dependency graph.
fn side_of(step: &Step) -> Option<Side> {
    match &step.item {
        Item::Bench { side, .. } => Some(*side),
        _ => None,
    }
}

/// Build the missing benches of each side in one cargo invocation.
fn warm(steps: &[Step], live: &Live) {
    for side in [Side::Head, Side::Base] {
        let missing: Vec<String> = steps
            .iter()
            .filter(|s| s.action == Action::Build && side_of(s) == Some(side))
            .map(|s| s.item.pkg().to_string())
            .collect();
        // The gate warms its builds the same way: one invocation overlaps the
        // packages' critical paths, which one-by-one builds serialize. Each
        // package is then built alone below, so a stored executable never
        // depends on this pass; `--features` names one package's features,
        // so with it that pass is skipped.
        if !missing.is_empty() && live.common.features.is_none() {
            let (dir, target_dir) = live.tree(side);
            let label = format!("cargo bench build {side:?}");
            if let Err(e) = invoke::timed(&label, || {
                invoke::prebuild_benches(&missing, live.common, dir, target_dir)
            }) {
                eprintln!("prebuild: the joint build failed ({e}); building each alone");
            }
        }
    }
}

pub(super) fn execute(steps: &[Step], live: &Live) -> Vec<Outcome> {
    warm(steps, live);
    steps
        .iter()
        .map(|s| {
            if s.action == Action::Covered {
                live.stage_reference(s.item.pkg());
                return Outcome::Covered;
            }
            let wants_build = s.action == Action::Build || side_of(s).is_some();
            let label = format!("prebuild {}", s.item);
            let built = if wants_build {
                invoke::timed(&label, || build_item(s, live))
            } else {
                Ok(None)
            };
            let outcome = match &built {
                Err(e) => Outcome::Failed(e.clone()),
                Ok(_) if s.action == Action::Stored => Outcome::Stored,
                Ok(_) if live.stored(&s.item) => Outcome::Built,
                Ok(_) => Outcome::Unstored,
            };
            if let Some(stage) = live.stage
                && matches!(outcome, Outcome::Built | Outcome::Stored)
            {
                stage_item(&s.item, built.ok().flatten(), live, stage);
            }
            outcome
        })
        .collect()
}
