//! Baseline files: where they live, how a run is scoped into one, and saving
//! and loading them.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{CommonArgs, Run, saved_items, workspace_root};

pub fn baseline_path(root: &Path, name: &str) -> PathBuf {
    root.join(".soothfast")
        .join("baselines")
        .join(format!("{name}.json"))
}

/// What a measurement run covered, deciding which stale baseline entries a
/// save may drop. One baseline file is shared by bench runs of several
/// packages plus buildcost runs, so each full run replaces only its own
/// slice; a filtered run updates in place and can leave stale ids behind
/// (gate's --filter exemption mirrors this).
pub enum SaveScope {
    /// Full bench run of one package (ids are package-qualified) — or of the
    /// whole workspace when no `-p` was given (`None` replaces every bench id).
    BenchFull(Option<String>),
    BenchFiltered(Option<String>),
    /// Buildcost run of one package (`None`: replace every buildcost entry).
    Buildcost(Option<String>),
    /// Import under `<prefix>`: replaces the items it marked with that prefix.
    Imported(String),
}

impl SaveScope {
    /// Scope of the run these args produced (what `--save-baseline` may
    /// replace) — shared by `measure` and `gate`.
    pub fn of(common: &CommonArgs) -> SaveScope {
        if common.backend.as_deref() == Some("buildcost") {
            SaveScope::Buildcost(common.pkg.clone())
        } else if common.filter.is_some() {
            SaveScope::BenchFiltered(common.pkg.clone())
        } else {
            SaveScope::BenchFull(common.pkg.clone())
        }
    }
}

/// First segment of a measured-item id (the package, `-` normalized to `_`).
pub fn id_pkg(id: &str) -> &str {
    id.split("::").next().unwrap_or(id)
}

fn is_imported(item: &Value) -> bool {
    item["imported"].is_string()
}

/// Persist a run as a named baseline.
pub fn save_baseline(name: &str, run: &Run, scope: SaveScope) -> io::Result<PathBuf> {
    save_baseline_in(&workspace_root()?, name, run, scope)
}

/// Persist a run as a named baseline under `root`.
pub fn save_baseline_in(
    root: &Path,
    name: &str,
    run: &Run,
    scope: SaveScope,
) -> io::Result<PathBuf> {
    let mut new_items = saved_items(run);
    if let SaveScope::BenchFull(Some(pkg)) | SaveScope::BenchFiltered(Some(pkg)) = &scope {
        for item in new_items
            .as_object_mut()
            .into_iter()
            .flat_map(|m| m.values_mut())
        {
            item["saved_by"] = json!(pkg);
        }
    }
    let measured = match &scope {
        SaveScope::BenchFull(pkg) => Some(pkg.clone()),
        _ => None,
    };
    let mut doc =
        load_baseline_in(root, name)?.unwrap_or_else(|| json!({ "version": 1, "items": {} }));
    let prior = own_measured(&mut doc, &scope, run)?;
    let unix_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    doc["created_unix"] = json!(unix_now);
    if let Some(n) = run.noise_pct {
        doc["noise_pct"] = json!(n);
    }
    if let Some(b) = &run.build {
        doc["build"] = b.to_json();
    }
    if let (Some(map), Some(new_map)) = (doc["items"].as_object_mut(), new_items.as_object()) {
        // Full runs replace their scope: renamed/removed items must not
        // haunt the baseline (the gate fails on GONE entries).
        match scope {
            SaveScope::BenchFull(Some(_)) => map.retain(|k, _| !prior.contains(k)),
            SaveScope::BenchFull(None) => {
                map.retain(|k, v| is_imported(v) || k.starts_with("buildcost::"))
            }
            SaveScope::Buildcost(Some(pkg)) => map.retain(|k, _| {
                k.strip_prefix("buildcost::")
                    .is_none_or(|rest| rest.split("::").next() != Some(pkg.as_str()))
            }),
            SaveScope::Buildcost(None) => map.retain(|k, _| !k.starts_with("buildcost::")),
            SaveScope::Imported(prefix) => {
                map.retain(|_, v| v["imported"].as_str() != Some(prefix.as_str()))
            }
            SaveScope::BenchFiltered(_) => {}
        }
        for (k, v) in new_map {
            map.insert(k.clone(), v.clone());
        }
    }
    // Assertion verdicts ride along so renderers can show *verified* claims
    // ("proven O(n log n), zero-alloc") from produced data. Same scope rules
    // as items: a full run replaces its slice, keyed by the assertion's id.
    let replaced: Vec<String> = run.items.keys().chain(&prior).cloned().collect();
    let mut assertions: Vec<Value> = doc["assertions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| match a["id"].as_str().filter(|id| !id.is_empty()) {
            Some(id) => !replaced.contains(&id.to_string()),
            None => !run.assertions.iter().any(|n| {
                n.id.is_empty() && a["kind"] == n.kind.as_str() && a["detail"] == n.detail.as_str()
            }),
        })
        .collect();
    for a in &run.assertions {
        assertions.push(a.to_json());
    }
    doc["assertions"] = Value::Array(assertions);
    if let Some(pkg) = measured {
        crate::unmeasured::clear(&mut doc, pkg.as_deref());
    }

    write_baseline_in(root, name, &doc)?;
    prune_absorbed_accepts(root, run);
    Ok(baseline_path(root, name))
}

/// For a `-p` bench save, record in `doc` that the package owns what `run`
/// measured, and return what it owned before and no other package owns,
/// which a full save replaces. A filtered save adds to its owners and
/// replaces nothing beyond its own ids.
fn own_measured(doc: &mut Value, scope: &SaveScope, run: &Run) -> io::Result<BTreeSet<String>> {
    let (pkg, full) = match scope {
        SaveScope::BenchFull(Some(pkg)) => (pkg, true),
        SaveScope::BenchFiltered(Some(pkg)) => (pkg, false),
        _ => return Ok(BTreeSet::new()),
    };
    let bench = |id: &str, v: &Value| !is_imported(v) && !id.starts_with("buildcost::");
    let prior: BTreeSet<String> = if full && doc.get(crate::unmeasured::OWNERS_KEY).is_none() {
        let legacy: BTreeSet<String> = doc["items"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(id, v)| bench(id, v))
            .map(|(id, _)| id.clone())
            .collect();
        if !legacy.is_empty() {
            println!(
                "baseline: it predates item ownership, so this save of {pkg} replaces every bench item in it; re-save the other packages"
            );
        }
        legacy
    } else {
        crate::unmeasured::owned_by(doc, pkg).into_iter().collect()
    };
    let mut owned: BTreeSet<String> = if full { BTreeSet::new() } else { prior.clone() };
    owned.extend(
        run.items
            .iter()
            .filter(|(id, m)| m.imported.is_none() && !id.starts_with("buildcost::"))
            .map(|(id, _)| id.clone()),
    );
    let owned: Vec<String> = owned.into_iter().collect();
    let others = crate::unmeasured::owned_by_others(doc, pkg);
    crate::unmeasured::set_owners(doc, pkg, &owned);
    if !full {
        return Ok(BTreeSet::new());
    }
    Ok(prior
        .into_iter()
        .filter(|id| !others.contains(id))
        .collect())
}

/// Write `doc` as baseline `name` under `root`.
pub fn write_baseline_in(root: &Path, name: &str, doc: &Value) -> io::Result<()> {
    let path = baseline_path(root, name);
    std::fs::create_dir_all(path.parent().expect("baseline path has a parent"))?;
    std::fs::write(&path, serde_json::to_string_pretty(doc)?)
}

/// Drop `soothfast-gate.lock` entries a fresh baseline has absorbed. Best
/// effort: a baseline save must never fail because of the lock file.
fn prune_absorbed_accepts(root: &Path, run: &Run) {
    let Ok(entries) = crate::gate_lock::read(root) else {
        return;
    };
    let (pruned, removed) = crate::gate_lock::prune_absorbed(&entries, run);
    if removed.is_empty() {
        return;
    }
    if crate::gate_lock::write(root, &pruned).is_ok() {
        for id in removed {
            println!("soothfast-gate.lock: {id} absorbed into the new baseline, entry removed");
        }
    }
}

/// Load a named baseline, if present.
pub fn load_baseline(name: &str) -> io::Result<Option<Value>> {
    load_baseline_in(&workspace_root()?, name)
}

/// Load a named baseline under `root`, if present.
pub fn load_baseline_in(root: &Path, name: &str) -> io::Result<Option<Value>> {
    let path = baseline_path(root, name);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&text)?))
}

#[cfg(test)]
mod tests {
    use super::{CommonArgs, SaveScope};

    #[test]
    fn save_scope_mirrors_what_the_args_measured() {
        let mut common = CommonArgs {
            pkg: Some("mylib".into()),
            ..CommonArgs::default()
        };
        assert!(matches!(
            SaveScope::of(&common), SaveScope::BenchFull(Some(p)) if p == "mylib"
        ));
        common.filter = Some("group::".into());
        assert!(matches!(
            SaveScope::of(&common),
            SaveScope::BenchFiltered(_)
        ));
        common.filter = None;
        common.backend = Some("buildcost".into());
        assert!(matches!(
            SaveScope::of(&common), SaveScope::Buildcost(Some(p)) if p == "mylib"
        ));
    }
}
