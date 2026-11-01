//! Ids two packages of one baseline both own. A lib's measured fn is
//! linked into every dependent's bench binary, so each dependent's listing
//! shows it. One invariant holds: a number in `items` is credited to a
//! package only when that package saved it (`saved_by`). The package that
//! registered an id (the item's `pkg`, from the listing of the binary that
//! ran, recorded under `registered`) takes it from the others; a save by
//! another package never writes it. Two packages that both registered an
//! id clash, and so does an id no owner registered: `owners` names them and
//! `items` keeps no number for it. Once one package alone owns it again,
//! that package's next save measures it again.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::invoke::Run;
use crate::unmeasured::{self, OWNERS_KEY};

/// Each package's ids its own code registered, as its saves' listings said.
pub const REGISTERED_KEY: &str = "registered";

/// The ids each package listed in this run, for the packages it gated.
pub type Gated = BTreeMap<String, BTreeSet<String>>;

/// The keys an ambiguous id's item keeps: what names it, never a number.
const IDENTITY: [&str; 3] = ["fingerprint", "covers", "pkg"];

fn norm(pkg: &str) -> String {
    pkg.replace('-', "_")
}

/// The ids `map` (`{package: [ids]}`) lists under `pkg`, `-` read as `_`.
fn under(map: &Value, pkg: &str) -> BTreeSet<String> {
    let entries = map.as_object().into_iter().flatten();
    entries
        .filter(|(p, _)| norm(p) == norm(pkg))
        .flat_map(|(_, ids)| ids.as_array().into_iter().flatten())
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect()
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

/// The ids of `run` that `writer`'s save must not write a number for: one
/// another package registered, one whose registering package already owns
/// it, and, with no package named, one another package already owns.
pub fn foreign(doc: &Value, writer: &str, run: &Run) -> BTreeSet<String> {
    let listers = listers(doc);
    let registered = doc[REGISTERED_KEY].as_object().into_iter().flatten();
    let by_others: BTreeSet<String> = registered
        .filter(|(p, _)| norm(p) != norm(writer))
        .flat_map(|(_, ids)| ids.as_array().into_iter().flatten())
        .filter_map(|id| id.as_str().map(str::to_string))
        .collect();
    let others_own =
        |id: &str| (listers.get(id).into_iter().flatten()).any(|p| norm(p) != norm(writer));
    let items = run.items.iter().filter(|(_, m)| m.imported.is_none());
    items
        .filter(|(id, m)| {
            by_others.contains(*id)
                || match m.pkg.as_deref() {
                    Some(r) if norm(r) != norm(writer) => under(&doc[OWNERS_KEY], r).contains(*id),
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
/// `pkg` its listing named: all of them for a `full` save, else added to
/// those recorded before, less ids it shows another package registered. A
/// run whose listing named no package keeps what was recorded.
pub fn record_registered(doc: &mut Value, pkg: &str, run: &Run, full: bool) {
    if !run.items.values().any(|m| m.pkg.is_some()) {
        return;
    }
    let mut ids = if full {
        BTreeSet::new()
    } else {
        under(&doc[REGISTERED_KEY], pkg)
    };
    for (id, m) in &run.items {
        match m.pkg.as_deref() {
            Some(r) if norm(r) == norm(pkg) => {
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
    doc[REGISTERED_KEY][pkg] = json!(ids);
}

/// Settle `doc`'s shared ids. One owner that registered an id takes it from
/// the others; with none recorded, the item's `pkg` does, when that package
/// owns it or `gated` shows it listing it. The id keeps its number only if
/// the package taking it saved that number. An id still owned by several
/// packages keeps no number and no verdict.
pub fn settle(doc: &mut Value, gated: &Gated) {
    let mut listers = listers(doc);
    let mut unvouched = BTreeSet::new();
    for (id, packages) in listers.iter_mut().filter(|(_, p)| p.len() > 1) {
        let selves: Vec<String> = (packages.iter())
            .filter(|p| under(&doc[REGISTERED_KEY], p).contains(id.as_str()))
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
        let saver = doc["items"][id.as_str()]["saved_by"].as_str().map(norm);
        if saver.as_deref() != Some(registrar.as_str()) {
            unvouched.insert(id.clone());
        }
    }
    unmeasured::drop_items(doc, &unvouched);
    let ambiguous: BTreeSet<String> = (listers.into_iter())
        .filter(|(_, p)| p.len() > 1)
        .map(|(id, _)| id)
        .collect();
    if let Some(items) = doc.get_mut("items").and_then(Value::as_object_mut) {
        let shared = items.iter_mut().filter(|(id, _)| ambiguous.contains(*id));
        for fields in shared.filter_map(|(_, item)| item.as_object_mut()) {
            fields.retain(|k, _| IDENTITY.contains(&k.as_str()));
        }
    }
    if let Some(assertions) = doc.get_mut("assertions").and_then(Value::as_array_mut) {
        assertions.retain(|a| a["id"].as_str().is_none_or(|id| !ambiguous.contains(id)));
    }
}

/// Take out of the gate's `baseline` every item of `run` the baseline marks
/// ambiguous, so the gate reads it as having no baseline number, and say so.
pub fn unbaselined(baseline: &mut Value, run: &Run) -> Vec<String> {
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
