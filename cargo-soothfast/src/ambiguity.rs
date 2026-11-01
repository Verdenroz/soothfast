//! Ids two packages of one baseline both own. A lib's measured fn is
//! linked into every dependent's bench binary, so each dependent's listing
//! shows it. The package that registered it (the item's `pkg`) takes it
//! from the others when it owns the id too, or when this run gated it and
//! it listed the id. Otherwise, as for a feature-gated item or a lib with no
//! bench target, each listing package keeps it, and the id is ambiguous:
//! its numbers move out of `items` into the `ambiguous` side map, which
//! readers that predate it never see. No number is ever thrown away while a
//! package that may have written it still owns the id.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use soothfast_docs::baseline::AMBIGUOUS_KEY;

use crate::unmeasured::OWNERS_KEY;

/// The ids each package listed in this run, for the packages it gated.
pub type Gated = BTreeMap<String, BTreeSet<String>>;

/// Each owned id and the packages that own it.
pub type Listers = BTreeMap<String, Vec<String>>;

fn norm(pkg: &str) -> String {
    pkg.replace('-', "_")
}

/// Each owned id of `doc` and its owners.
pub fn listers(doc: &Value) -> Listers {
    let mut out = Listers::new();
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

/// Before `writer` writes `ids`' numbers into `doc`, set aside each number
/// already there whose id another package owns, under the package that
/// wrote it: the id's one other owner, or, with several, all of them.
pub fn stash_before_write<'a>(
    doc: &mut Value,
    writer: &str,
    ids: impl Iterator<Item = &'a String>,
) {
    let listers = listers(doc);
    for id in ids {
        let others: Vec<String> = (listers.get(id).into_iter().flatten())
            .filter(|p| *p != writer)
            .cloned()
            .collect();
        match others.as_slice() {
            [] => {}
            [only] => stash(doc, id, Some(only), &others),
            _ => stash(doc, id, None, &others),
        }
    }
}

/// Recompute `doc`'s ambiguous ids from its owners. `writer` is the package
/// whose numbers `doc`'s items just took, if one did; `gated` what this
/// run's gated packages listed.
pub fn settle(doc: &mut Value, writer: Option<&str>, gated: &Gated) {
    for (id, packages) in listers(doc).into_iter().filter(|(_, p)| p.len() > 1) {
        let known = writer.filter(|w| packages.iter().any(|p| p == w));
        stash(doc, &id, known, &packages);
    }
    for (id, packages) in listers(doc) {
        let Some(registrar) = registrar(doc, &id).map(|r| norm(&r)) else {
            continue;
        };
        let owns = packages.iter().any(|p| norm(p) == registrar)
            || (gated.iter()).any(|(p, ids)| norm(p) == registrar && ids.contains(&id));
        if owns {
            for p in packages.iter().filter(|p| norm(p) != registrar) {
                disown(doc, p, &id);
            }
        }
    }
    resolve(doc, &listers(doc));
}

/// Drop `package`'s set-aside numbers of `ids`, which a failed gate of it no
/// longer vouches for.
pub fn forget(doc: &mut Value, package: &str, ids: &BTreeSet<String>) {
    for id in ids {
        if let Some(saved) = doc[AMBIGUOUS_KEY][id].as_array_mut() {
            saved.retain(|s| s["package"] != package);
        }
    }
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

/// Move `id`'s number, if a measured one, from `items` to the side map, as
/// written by `writer`, or, when nothing names it, by one of `owners`.
fn stash(doc: &mut Value, id: &str, writer: Option<&str>, owners: &[String]) {
    if doc["items"][id]["imported"].is_string() {
        return;
    }
    let Some(item) = doc["items"].as_object_mut().and_then(|m| m.remove(id)) else {
        return;
    };
    let saved = match writer {
        Some(package) => json!({ "package": package, "item": item }),
        None => json!({ "owners": owners, "item": item }),
    };
    if !doc[AMBIGUOUS_KEY].is_object() {
        doc[AMBIGUOUS_KEY] = json!({});
    }
    let slot = &mut doc[AMBIGUOUS_KEY][id];
    if let Some(list) = slot.as_array_mut() {
        if let Some(package) = writer {
            list.retain(|s| s["package"] != package);
        }
        list.push(saved);
    } else {
        *slot = json!([saved]);
    }
}

/// The package that registered `id`, as its number or a set-aside one says.
fn registrar(doc: &Value, id: &str) -> Option<String> {
    let set_aside = doc[AMBIGUOUS_KEY][id].as_array().into_iter().flatten();
    std::iter::once(&doc["items"][id])
        .chain(set_aside.map(|s| &s["item"]))
        .find_map(|item| item["pkg"].as_str().map(str::to_string))
}

fn disown(doc: &mut Value, package: &str, id: &str) {
    if let Some(ids) = doc[OWNERS_KEY][package].as_array_mut() {
        ids.retain(|owned| owned != id);
    }
    if let Some(saved) = doc[AMBIGUOUS_KEY][id].as_array_mut() {
        saved.retain(|s| s["package"] != package);
    }
}

/// Whether set-aside number `saved` may be its writer's while `owners` own
/// its id.
fn vouched(saved: &Value, owners: &[String]) -> bool {
    match saved["package"].as_str() {
        Some(package) => owners.iter().any(|o| o == package),
        None => (saved["owners"].as_array().into_iter().flatten())
            .any(|p| owners.iter().any(|o| p == o)),
    }
}

/// Keep a set-aside number only while a package that may have written it
/// owns its id, and give an id one package owns again that package's number:
/// the one it wrote, else one whose writer nothing names but it owned.
fn resolve(doc: &mut Value, listers: &Listers) {
    let Some(map) = doc.get_mut(AMBIGUOUS_KEY).and_then(Value::as_object_mut) else {
        return;
    };
    let mut restored = Vec::new();
    map.retain(|id, saved| {
        let owners = listers.get(id).map(Vec::as_slice).unwrap_or_default();
        let Some(saved) = saved.as_array_mut() else {
            return false;
        };
        saved.retain(|s| vouched(s, owners));
        if let [only] = owners {
            let own = (saved.iter()).position(|s| s["package"] == only.as_str());
            let pick = own.or_else(|| (saved.iter()).position(|s| s["package"].is_null()));
            if let Some(i) = pick {
                restored.push((id.clone(), saved.swap_remove(i)["item"].take()));
            }
            return false;
        }
        owners.len() > 1 && !saved.is_empty()
    });
    if map.is_empty()
        && let Some(d) = doc.as_object_mut()
    {
        d.remove(AMBIGUOUS_KEY);
    }
    for (id, item) in restored {
        if doc["items"][&id].is_null() {
            doc["items"][&id] = item;
        }
    }
}

#[cfg(test)]
mod tests;
