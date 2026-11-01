//! The items of every merged file, and the numbers of an id two Rust files
//! both own, which go to the side map for [`crate::ambiguity`] to settle.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use soothfast_docs::baseline::AMBIGUOUS_KEY;

use crate::unmeasured::OWNERS_KEY;

/// Where an id was first seen: its file, and whether that file owns it.
struct Origin {
    label: String,
    imported: bool,
    owned: bool,
    owner: String,
}

#[derive(Default)]
pub struct Collected {
    pub items: Map<String, Value>,
    pub side: Map<String, Value>,
    origin: BTreeMap<String, Origin>,
}

/// The packages of `doc` that own `id`.
fn owners_of(doc: &Value, id: &str) -> Vec<String> {
    let owners = doc[OWNERS_KEY].as_object().into_iter().flatten();
    owners
        .filter(|(_, ids)| ids.as_array().into_iter().flatten().any(|i| i == id))
        .map(|(p, _)| p.clone())
        .collect()
}

impl Collected {
    /// Take the items of `doc`, read from file `label`. An id in two files
    /// is refused unless both are Rust files whose owners list it; a Rust
    /// file holding an id none of its packages owns is refused.
    pub fn take(
        &mut self,
        label: &str,
        doc: &Value,
        items: &Map<String, Value>,
    ) -> Result<(), String> {
        for (id, saved) in doc[AMBIGUOUS_KEY].as_object().into_iter().flatten() {
            for (package, item) in saved.as_object().into_iter().flatten() {
                self.set_aside(id, package, item.clone());
            }
        }
        for (id, v) in items {
            let imported = v["imported"].is_string();
            let owners = owners_of(doc, id);
            let has_owners = doc.get(OWNERS_KEY).is_some();
            if !imported && has_owners && owners.is_empty() {
                return Err(format!("{label}: holds {id}, which no package of it owns"));
            }
            let owner = match owners.as_slice() {
                [only] => only.clone(),
                _ => String::new(),
            };
            let Some(first) = self.origin.get(id) else {
                let origin = Origin {
                    label: label.to_string(),
                    imported,
                    owned: !owners.is_empty(),
                    owner,
                };
                self.origin.insert(id.clone(), origin);
                self.items.insert(id.clone(), v.clone());
                continue;
            };
            if imported || first.imported || !first.owned || owners.is_empty() {
                return Err(format!("{label}: {id} is in {} too", first.label));
            }
            let first_owner = first.owner.clone();
            if let Some(prev) = self.items.remove(id) {
                self.set_aside(id, &first_owner, prev);
            }
            self.set_aside(id, &owner, v.clone());
        }
        Ok(())
    }

    fn set_aside(&mut self, id: &str, package: &str, item: Value) {
        let saved = self.side.entry(id).or_insert_with(|| json!({}));
        saved[package] = item;
    }
}
