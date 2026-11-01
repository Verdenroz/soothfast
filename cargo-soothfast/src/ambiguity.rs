//! Ids two packages of one baseline both own. A lib's measured fn is
//! linked into every dependent's bench binary, so each dependent's listing
//! shows it. The package that registered it (the item's `pkg`, from the
//! listing of the binary that ran) takes it from the others when it owns
//! the id too, or when this run gated it and it listed the id. Otherwise,
//! as for two packages registering the same id, every listing package keeps
//! it, and the id is ambiguous: `owners` names its packages and `items`
//! holds no number for it, since nothing says whose it would be. Once one
//! package alone owns it again, that package's next save measures it again.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::invoke::Run;
use crate::unmeasured::OWNERS_KEY;

/// Each package's ids its own code registered, as its saves' listings said.
pub const REGISTERED_KEY: &str = "registered";

/// The ids each package listed in this run, for the packages it gated.
pub type Gated = BTreeMap<String, BTreeSet<String>>;

fn norm(pkg: &str) -> String {
    pkg.replace('-', "_")
}

/// Each owned id of `doc` and its owners.
fn listers(doc: &Value) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (package, ids) in doc[OWNERS_KEY].as_object().into_iter().flatten() {
        let ids = ids
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        for id in ids {
            out.entry(id.to_string()).or_default().push(package.clone());
        }
    }
    out
}

/// Record in `doc` the ids of `run` that `pkg`'s own code registered: all
/// of them for a `full` save, else added to those recorded before.
pub fn record_registered(doc: &mut Value, pkg: &str, run: &Run, full: bool) {
    let mut ids: BTreeSet<String> = if full {
        BTreeSet::new()
    } else {
        let prior = doc[REGISTERED_KEY][pkg].as_array().into_iter().flatten();
        prior
            .filter_map(|i| i.as_str().map(str::to_string))
            .collect()
    };
    let own = run
        .items
        .iter()
        .filter(|(_, m)| m.pkg.as_deref().is_some_and(|p| norm(p) == norm(pkg)));
    ids.extend(own.map(|(id, _)| id.clone()));
    if !doc[REGISTERED_KEY].is_object() {
        doc[REGISTERED_KEY] = serde_json::json!({});
    }
    doc[REGISTERED_KEY][pkg] = serde_json::json!(ids);
}

/// Whether `pkg` registered `id`, as `doc` records.
fn registered(doc: &Value, pkg: &str, id: &str) -> bool {
    let ids = doc[REGISTERED_KEY][pkg].as_array().into_iter().flatten();
    ids.into_iter().any(|i| i == id)
}

/// Settle `doc`'s shared ids. One package that registered an id takes it
/// from the other owners; with none recorded, the item's `pkg` does, when
/// that package owns it or `gated` shows it listing it. Two packages that
/// both registered it clash. The number of an id still owned by several
/// packages is dropped, with its claims' verdicts.
pub fn settle(doc: &mut Value, gated: &Gated) {
    let mut listers = listers(doc);
    for (id, packages) in listers.iter_mut().filter(|(_, p)| p.len() > 1) {
        let selves: Vec<String> = (packages.iter())
            .filter(|p| registered(doc, p, id))
            .map(|p| norm(p))
            .collect();
        let registrar = match selves.as_slice() {
            [only] => only.clone(),
            [] => {
                let Some(registrar) = doc["items"][id.as_str()]["pkg"].as_str().map(norm) else {
                    continue;
                };
                let owns = packages.iter().any(|p| norm(p) == registrar)
                    || (gated.iter()).any(|(p, ids)| norm(p) == registrar && ids.contains(id));
                if !owns {
                    continue;
                }
                registrar
            }
            _ => continue,
        };
        for p in packages.iter().filter(|p| norm(p) != registrar) {
            if let Some(ids) = doc[OWNERS_KEY][p.as_str()].as_array_mut() {
                ids.retain(|owned| owned != id.as_str());
            }
        }
        packages.retain(|p| norm(p) == registrar);
    }
    let ambiguous: BTreeSet<&String> = (listers.iter())
        .filter(|(_, p)| p.len() > 1)
        .map(|(id, _)| id)
        .collect();
    if let Some(items) = doc.get_mut("items").and_then(Value::as_object_mut) {
        items.retain(|id, v| v["imported"].is_string() || !ambiguous.contains(id));
    }
    if let Some(assertions) = doc.get_mut("assertions").and_then(Value::as_array_mut) {
        assertions.retain(|a| {
            a["id"]
                .as_str()
                .is_none_or(|id| !ambiguous.contains(&id.to_string()))
        });
    }
}

/// The ids of `items` that `writer` is about to save but whose registering
/// package already owns them in `doc`: that package's number stands.
pub fn registered_elsewhere(doc: &Value, writer: &str, items: &Value) -> BTreeSet<String> {
    let items = items.as_object().into_iter().flatten();
    items
        .filter_map(|(id, item)| {
            let registrar = item["pkg"].as_str().filter(|r| norm(r) != norm(writer))?;
            let owned = doc[OWNERS_KEY][registrar]
                .as_array()?
                .iter()
                .any(|i| i == id.as_str());
            owned.then(|| id.clone())
        })
        .collect()
}

/// Take out of the gate's `baseline` every item of `run` the baseline marks
/// ambiguous, so the gate reads it as having no baseline number, and say so.
pub fn unbaselined(baseline: &mut Value, run: &crate::invoke::Run) -> Vec<String> {
    let mut notes = Vec::new();
    for id in run.items.keys() {
        let packages = soothfast_docs::baseline::ambiguous_packages(baseline, id);
        if packages.is_empty() {
            continue;
        }
        if let Some(items) = baseline["items"].as_object_mut() {
            items.remove(id);
        }
        notes.push(format!(
            "gate: {id} has no baseline number: it is ambiguous between {}",
            packages.join(" and ")
        ));
    }
    notes
}

#[cfg(test)]
mod tests;
