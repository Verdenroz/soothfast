//! What a passing gate's `--save-baseline` writes: HEAD's run when this gate
//! measured it in full, otherwise its items as `unmeasured` with the reason.
//! A `--shard` gate also stamps the file with its shard and the items its
//! bench binary lists, which `baseline merge` needs to name a missing shard's.

use std::path::Path;

use crate::baseline_merge;
use crate::invoke::{self, CommonArgs, Run};
use crate::unmeasured::{self, Reason};

/// Where a gate's HEAD run came from, which decides whether it is baseline
/// material.
#[derive(Debug, PartialEq)]
pub enum HeadRun {
    /// Measured by this gate, gating counters included.
    Measured,
    /// The identical-binaries short circuit: one pass without the counters.
    Uncounted,
    /// Read from the run cache, labelled with what it was measured from.
    Stored(String),
}

/// What a `--save-baseline` request writes. A failing run writes nothing,
/// since saving it would ratify its own regression.
#[derive(Debug, PartialEq)]
pub enum SaveVerdict {
    Save,
    RefuseFailing,
    Unmeasured(Reason, String),
}

pub fn save_verdict(failures: u32, head: &HeadRun) -> SaveVerdict {
    match head {
        _ if failures > 0 => SaveVerdict::RefuseFailing,
        HeadRun::Measured => SaveVerdict::Save,
        HeadRun::Uncounted => SaveVerdict::Unmeasured(
            Reason::IdenticalBinaries,
            "bench binaries identical to the merge-base, so the gate took no gating counters"
                .into(),
        ),
        HeadRun::Stored(from) => SaveVerdict::Unmeasured(
            Reason::StoredRun,
            format!("HEAD's run came from the run cache ({from}), not from this gate"),
        ),
    }
}

/// Act on a `--save-baseline NAME` request under `root` as `verdict` decides.
pub fn save_head(
    root: &Path,
    name: &str,
    current: &Run,
    common: &CommonArgs,
    verdict: SaveVerdict,
) -> Result<(), String> {
    if verdict == SaveVerdict::RefuseFailing {
        eprintln!("soothfast: refusing to save baseline {name:?}: gate failed");
        return Ok(());
    }
    let shard = match common.shard {
        Some(shard) => {
            baseline_merge::check_shard_in(root, name, shard)?;
            Some((shard, listed_ids(common)?))
        }
        None => None,
    };
    match verdict {
        SaveVerdict::Unmeasured(reason, detail) => {
            let entries = unmeasured::by_package(reason, &detail, current.items.keys());
            unmeasured::mark_in(root, name, &entries).map_err(|e| e.to_string())?;
            for e in &entries {
                println!(
                    "gate: baseline {name:?} holds no measurement of {} item(s) of {}: {detail}",
                    e.items.len(),
                    e.package
                );
            }
        }
        _ => {
            let scope = invoke::SaveScope::of(common);
            let path =
                invoke::save_baseline_in(root, name, current, scope).map_err(|e| e.to_string())?;
            println!("baseline saved: {}", path.display());
        }
    }
    if let Some((shard, listed)) = shard {
        baseline_merge::stamp_shard_in(root, name, shard, &listed).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Every item the bench binary lists, whichever shard owns it.
fn listed_ids(common: &CommonArgs) -> Result<Vec<String>, String> {
    invoke::bench_executable(common, None, None)
        .and_then(|exe| invoke::list_items(&exe, common.filter.as_deref()))
        .map(|items| items.into_keys().collect())
        .ok_or_else(|| "cannot list the bench binary's items for the shard's baseline".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_run_is_never_saved() {
        assert_eq!(
            save_verdict(1, &HeadRun::Measured),
            SaveVerdict::RefuseFailing
        );
        assert_eq!(
            save_verdict(3, &HeadRun::Uncounted),
            SaveVerdict::RefuseFailing
        );
        assert_eq!(
            save_verdict(2, &HeadRun::Stored("abc".into())),
            SaveVerdict::RefuseFailing
        );
    }

    #[test]
    fn only_a_run_this_gate_measured_in_full_is_saved() {
        assert_eq!(save_verdict(0, &HeadRun::Measured), SaveVerdict::Save);
        assert!(matches!(
            save_verdict(0, &HeadRun::Uncounted),
            SaveVerdict::Unmeasured(Reason::IdenticalBinaries, _)
        ));
        assert!(matches!(
            save_verdict(0, &HeadRun::Stored("abc".into())),
            SaveVerdict::Unmeasured(Reason::StoredRun, _)
        ));
    }

    #[test]
    fn an_uncounted_pass_lists_its_items_as_unmeasured() {
        let root = std::env::temp_dir().join(format!("soothfast-uncounted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut run = Run::default();
        run.items.insert("core::a".into(), Default::default());
        let common = CommonArgs {
            pkg: Some("core".into()),
            ..CommonArgs::default()
        };
        let verdict = save_verdict(0, &HeadRun::Uncounted);
        save_head(&root, "head", &run, &common, verdict).unwrap();
        let doc = invoke::load_baseline_in(&root, "head").unwrap().unwrap();
        let entries = unmeasured::read(&doc).unwrap();
        assert_eq!(entries[0].items, ["core::a"]);
        assert_eq!(entries[0].reason, Reason::IdenticalBinaries);
        assert_eq!(doc["items"], serde_json::json!({}));
        let _ = std::fs::remove_dir_all(&root);
    }
}
