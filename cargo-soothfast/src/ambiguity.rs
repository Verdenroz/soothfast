//! Ids two packages of one baseline both own. A lib's measured fn is
//! linked into every dependent's bench binary, so each dependent's listing
//! shows it. The package that registered it (the item's `pkg`, from the
//! listing of the binary that ran) takes it from the others when it owns
//! the id too, or when this run gated it and it listed the id. Otherwise,
//! as for a feature-gated item or a lib with no bench target, each listing
//! package keeps it, and the id is ambiguous: its numbers move out of
//! `items` into the `ambiguous` side map, `{id: {package: item}}`, which
//! readers that predate it never see, and an `ambiguous-id` entry names it.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use serde_json::{Value, json};
use soothfast_docs::baseline::AMBIGUOUS_KEY;

use crate::unmeasured::{self, OWNERS_KEY, Reason, Unmeasured};

/// The ids each package listed in this run, for the packages it gated.
pub type Gated = BTreeMap<String, BTreeSet<String>>;

fn norm(pkg: &str) -> String {
    pkg.replace('-', "_")
}

/// Before `writer` writes `id`'s number into `doc`, set aside the number
/// already there when another package owns the id: under that package when
/// one does, else under `""`, a writer nothing can name.
pub fn stash_before_write(doc: &mut Value, writer: &str, id: &str) {
    let others: Vec<String> = listers(doc)
        .remove(id)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p != writer)
        .collect();
    if others.is_empty() {
        return;
    }
    let key = match others.as_slice() {
        [only] => only.as_str(),
        _ => "",
    };
    stash(doc, id, key);
}

/// Recompute `doc`'s ambiguous ids from its owners. `writer` is the package
/// whose numbers `doc`'s items just took, if one did; `gated` what this
/// run's gated packages listed. Fails, changing nothing, on a malformed
/// `unmeasured` entry.
pub fn settle(doc: &mut Value, writer: Option<&str>, gated: &Gated) -> io::Result<()> {
    let mut entries: Vec<Unmeasured> = unmeasured::read(doc).map_err(io::Error::other)?;
    entries.retain(|e| e.reason != Reason::AmbiguousId);
    for (id, packages) in listers(doc) {
        if packages.len() > 1 {
            let key = writer.filter(|w| packages.iter().any(|p| p == w));
            stash(doc, &id, key.unwrap_or(""));
        }
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
    let listers = listers(doc);
    resolve(doc, &listers);
    for (id, packages) in listers.into_iter().filter(|(_, p)| p.len() > 1) {
        let detail = format!(
            "item id {id} is measured by both {}; a claim on it is ambiguous",
            packages.join(" and ")
        );
        let ambiguous =
            unmeasured::entry(&packages[0], Reason::AmbiguousId, &detail, &[], vec![id]);
        entries.push(ambiguous);
    }
    doc["unmeasured"] = entries.iter().map(Unmeasured::to_json).collect();
    Ok(())
}

/// Drop `package`'s set-aside numbers of `ids`, which a failed gate of it no
/// longer vouches for.
pub fn forget(doc: &mut Value, package: &str, ids: &BTreeSet<String>) {
    let Some(map) = doc.get_mut(AMBIGUOUS_KEY).and_then(Value::as_object_mut) else {
        return;
    };
    for id in ids {
        if let Some(saved) = map.get_mut(id).and_then(Value::as_object_mut) {
            saved.remove(package);
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

/// Move `id`'s number, if a measured one, from `items` to the side map
/// under `key`.
fn stash(doc: &mut Value, id: &str, key: &str) {
    if doc["items"][id]["imported"].is_string() {
        return;
    }
    let Some(number) = doc["items"].as_object_mut().and_then(|m| m.remove(id)) else {
        return;
    };
    if !doc[AMBIGUOUS_KEY].is_object() {
        doc[AMBIGUOUS_KEY] = json!({});
    }
    if !doc[AMBIGUOUS_KEY][id].is_object() {
        doc[AMBIGUOUS_KEY][id] = json!({});
    }
    doc[AMBIGUOUS_KEY][id][key] = number;
}

/// The package that registered `id`, as its number or a set-aside one says.
fn registrar(doc: &Value, id: &str) -> Option<String> {
    let set_aside = doc[AMBIGUOUS_KEY][id].as_object().into_iter().flatten();
    std::iter::once(&doc["items"][id])
        .chain(set_aside.map(|(_, item)| item))
        .find_map(|item| item["pkg"].as_str().map(str::to_string))
}

fn disown(doc: &mut Value, package: &str, id: &str) {
    if let Some(ids) = doc[OWNERS_KEY][package].as_array_mut() {
        ids.retain(|owned| owned != id);
    }
    if let Some(saved) = doc[AMBIGUOUS_KEY][id].as_object_mut() {
        saved.remove(package);
    }
}

/// Keep a set-aside number only for a package still owning its id, and give
/// an id one package owns again its number back.
fn resolve(doc: &mut Value, listers: &BTreeMap<String, Vec<String>>) {
    let Some(map) = doc.get_mut(AMBIGUOUS_KEY).and_then(Value::as_object_mut) else {
        return;
    };
    let mut restored = Vec::new();
    map.retain(|id, saved| {
        let owners = listers.get(id).map(Vec::as_slice).unwrap_or_default();
        let Some(saved) = saved.as_object_mut() else {
            return false;
        };
        if let [only] = owners {
            if let Some(number) = saved.remove(only) {
                restored.push((id.clone(), number));
            }
            return false;
        }
        saved.retain(|p, _| p.is_empty() || owners.contains(p));
        owners.len() > 1 && !saved.is_empty()
    });
    if map.is_empty()
        && let Some(d) = doc.as_object_mut()
    {
        d.remove(AMBIGUOUS_KEY);
    }
    for (id, number) in restored {
        if doc["items"][&id].is_null() {
            doc["items"][&id] = number;
        }
    }
}

#[cfg(test)]
mod tests;
