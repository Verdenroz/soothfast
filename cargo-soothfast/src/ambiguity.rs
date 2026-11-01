//! Ids two packages of one baseline both own. A lib's measured fn is
//! linked into every dependent's bench binary, so each dependent's listing
//! shows it. One invariant holds: a number in `items` is credited to a
//! package only when that package saved it (`saved_by`). The package that
//! registered an id (the item's `pkg`, from the listing of the binary that
//! ran, recorded under `registered`) takes it from the others; a save by
//! another package never writes it. Two packages that both registered an
//! id clash, and so does an id no owner registered: `items` holds nothing
//! for it, and the `ambiguous` index names its packages and what it covers.
//! Once one package alone owns it again, that package's next save measures
//! it again. Imported items belong to no package and are never settled.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};
use soothfast_docs::baseline::AMBIGUOUS_KEY;

use crate::invoke::Run;
use crate::unmeasured::{self, OWNERS_KEY, by_package, norm};

/// Each package's ids its own code registered, as its saves' listings said.
pub const REGISTERED_KEY: &str = "registered";

/// The ids each package listed in this run, for the packages it gated.
pub type Gated = BTreeMap<String, BTreeSet<String>>;

/// Each owned id of `doc` and its owners, as `owners` spells them.
fn listers(doc: &Value) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (package, ids) in doc[OWNERS_KEY].as_object().into_iter().flatten() {
        let ids = ids
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        for id in ids {
            let owners = out.entry(id.to_string()).or_default();
            if !owners.iter().any(|p| norm(p) == norm(package)) {
                owners.push(package.clone());
            }
        }
    }
    out
}

/// The ids of `run` that `writer`'s save must not write a number for: one
/// another package registered and still owns, one whose registering package
/// already owns it, and, with no package named, one another package owns.
pub fn foreign(doc: &Value, writer: &str, run: &Run) -> BTreeSet<String> {
    let writer = norm(writer);
    let owners = by_package(&doc[OWNERS_KEY]);
    let registered = by_package(&doc[REGISTERED_KEY]);
    let owns = |p: &str, id: &str| owners.get(p).is_some_and(|ids| ids.contains(id));
    let by_others = |id: &str| {
        (registered.iter()).any(|(p, ids)| *p != writer && ids.contains(id) && owns(p, id))
    };
    let others_own = |id: &str| (owners.iter()).any(|(p, ids)| *p != writer && ids.contains(id));
    let items = run.items.iter().filter(|(_, m)| m.imported.is_none());
    items
        .filter(|(id, m)| {
            by_others(id)
                || match m.pkg.as_deref().map(norm) {
                    Some(r) if r != writer => owns(&r, id),
                    Some(_) => false,
                    None => others_own(id),
                }
        })
        .map(|(id, _)| id.clone())
        .collect()
}

/// `run` less the items, verdicts and executed records of `ids`.
pub fn without(run: &Run, ids: &BTreeSet<String>) -> Run {
    let mut out = run.clone();
    out.items.retain(|id, _| !ids.contains(id));
    out.assertions.retain(|a| !ids.contains(&a.id));
    out.executed.retain(|id, _| !ids.contains(id));
    out
}

/// Record in `doc` the ids of `run` that `pkg`'s own code registered, by the
/// `pkg` its listing named: all of them for a `full` save, even none, else
/// added to those recorded before, less ids it shows another package
/// registered. A run with no listing keeps what was recorded.
pub fn record_registered(doc: &mut Value, pkg: &str, run: &Run, full: bool) {
    if !run.listed {
        return;
    }
    let key = norm(pkg);
    let mut ids = if full {
        BTreeSet::new()
    } else {
        by_package(&doc[REGISTERED_KEY])
            .remove(&key)
            .unwrap_or_default()
    };
    for (id, m) in &run.items {
        match m.pkg.as_deref().map(norm) {
            Some(r) if r == key => {
                ids.insert(id.clone());
            }
            Some(_) => {
                ids.remove(id);
            }
            None => {}
        }
    }
    if !doc[REGISTERED_KEY].is_object() {
        doc[REGISTERED_KEY] = json!({});
    }
    if let Some(map) = doc[REGISTERED_KEY].as_object_mut() {
        map.retain(|p, _| norm(p) != key);
        map.insert(pkg.to_string(), json!(ids));
    }
}

/// Settle `doc`'s shared ids. One owner that registered an id takes it from
/// the others; with none recorded, the item's `pkg` does, when that package
/// owns it or `gated` shows it listing it. The id keeps its number only if
/// the package taking it saved that number. An id still owned by several
/// packages leaves `items` and the verdicts for the `ambiguous` index.
pub fn settle(doc: &mut Value, gated: &Gated) {
    let imported: BTreeSet<String> = (doc["items"].as_object().into_iter().flatten())
        .filter(|(_, v)| v["imported"].is_string())
        .map(|(id, _)| id.clone())
        .collect();
    let registered = by_package(&doc[REGISTERED_KEY]);
    let gated: Gated = (gated.iter()).fold(Gated::new(), |mut out, (p, ids)| {
        out.entry(norm(p)).or_default().extend(ids.iter().cloned());
        out
    });
    let mut listers = listers(doc);
    listers.retain(|id, _| !imported.contains(id));
    let mut handed: BTreeMap<String, String> = BTreeMap::new();
    for (id, packages) in listers.iter_mut().filter(|(_, p)| p.len() > 1) {
        let selves: Vec<String> = (packages.iter())
            .map(|p| norm(p))
            .filter(|p| registered.get(p).is_some_and(|ids| ids.contains(id)))
            .collect();
        let registrar = match selves.as_slice() {
            [only] => only.clone(),
            [] => {
                let Some(registrar) = doc["items"][id.as_str()]["pkg"].as_str().map(norm) else {
                    continue;
                };
                let listed = gated.get(&registrar).is_some_and(|ids| ids.contains(id));
                if !listed && !packages.iter().any(|p| norm(p) == registrar) {
                    continue;
                }
                registrar
            }
            _ => continue,
        };
        packages.retain(|p| norm(p) == registrar);
        handed.insert(id.clone(), registrar);
    }
    if let Some(owners) = doc.get_mut(OWNERS_KEY).and_then(Value::as_object_mut) {
        for (p, ids) in owners.iter_mut() {
            let p = norm(p);
            if let Some(ids) = ids.as_array_mut() {
                ids.retain(|id| {
                    id.as_str()
                        .and_then(|id| handed.get(id))
                        .is_none_or(|r| *r == p)
                });
            }
        }
    }
    let unvouched: BTreeSet<String> = (handed.iter())
        .filter(|(id, r)| {
            doc["items"][id.as_str()]["saved_by"]
                .as_str()
                .map(norm)
                .as_ref()
                != Some(*r)
        })
        .map(|(id, _)| id.clone())
        .collect();
    unmeasured::drop_items(doc, &unvouched);
    listers.retain(|_, p| p.len() > 1);
    index(doc, &listers);
    prune_registered(doc);
}

/// Replace `doc`'s `ambiguous` index with `ambiguous` (id to packages),
/// taking each id's number and verdicts out of the baseline.
fn index(doc: &mut Value, ambiguous: &BTreeMap<String, Vec<String>>) {
    let prior = doc.get(AMBIGUOUS_KEY).cloned().unwrap_or(Value::Null);
    let mut index = Map::new();
    for (id, packages) in ambiguous {
        let mut entry = json!({ "packages": packages });
        let covers = doc["items"][id.as_str()]["covers"].as_str();
        if let Some(covers) = covers.or(prior[id.as_str()]["covers"].as_str()) {
            entry["covers"] = json!(covers);
        }
        index.insert(id.clone(), entry);
    }
    if let Some(items) = doc.get_mut("items").and_then(Value::as_object_mut) {
        items.retain(|id, _| !ambiguous.contains_key(id));
    }
    if let Some(assertions) = doc.get_mut("assertions").and_then(Value::as_array_mut) {
        assertions.retain(|a| {
            a["id"]
                .as_str()
                .is_none_or(|id| !ambiguous.contains_key(id))
        });
    }
    if let Some(fields) = doc.as_object_mut() {
        fields.remove(AMBIGUOUS_KEY);
        if !index.is_empty() {
            fields.insert(AMBIGUOUS_KEY.into(), Value::Object(index));
        }
    }
}

/// Drop from each package's `registered` ids the package no longer owns.
fn prune_registered(doc: &mut Value) {
    let owners = by_package(&doc[OWNERS_KEY]);
    let Some(registered) = doc.get_mut(REGISTERED_KEY).and_then(Value::as_object_mut) else {
        return;
    };
    for (p, ids) in registered.iter_mut() {
        let owned = owners.get(&norm(p));
        if let Some(ids) = ids.as_array_mut() {
            ids.retain(|id| {
                id.as_str()
                    .is_some_and(|id| owned.is_some_and(|o| o.contains(id)))
            });
        }
    }
}

/// A note for each item of `run` that `baseline` indexes as ambiguous, which
/// the gate compares as having no baseline number.
pub fn unbaselined(baseline: &Value, run: &Run) -> Vec<String> {
    let ids = run.items.keys();
    ids.filter_map(|id| {
        let packages = soothfast_docs::baseline::ambiguous_packages(baseline, id);
        (!packages.is_empty()).then(|| {
            format!(
                "{id} has no baseline number: ambiguous between {}",
                packages.join(", ")
            )
        })
    })
    .collect()
}

#[cfg(test)]
mod tests;
