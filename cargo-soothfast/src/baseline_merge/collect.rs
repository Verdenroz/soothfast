//! The items of every merged file, each from exactly one file.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::unmeasured::OWNERS_KEY;

#[derive(Default)]
pub struct Collected {
    pub items: Map<String, Value>,
    origin: BTreeMap<String, String>,
}

/// Whether some package of `doc` owns `id`.
fn owned(doc: &Value, id: &str) -> bool {
    let owners = doc[OWNERS_KEY].as_object().into_iter().flatten();
    owners
        .flat_map(|(_, ids)| ids.as_array().into_iter().flatten())
        .any(|i| i == id)
}

impl Collected {
    /// Take the items of `doc`, read from file `label`. An id another file
    /// holds too is refused, as is a bench id none of a file's packages
    /// owns. Imported and `buildcost::` ids are owned by no package, and a
    /// file with no `owners` predates them.
    pub fn take(
        &mut self,
        label: &str,
        doc: &Value,
        items: &Map<String, Value>,
    ) -> Result<(), String> {
        for (id, v) in items {
            let ownerless = v["imported"].is_string() || id.starts_with("buildcost::");
            if !ownerless && doc.get(OWNERS_KEY).is_some() && !owned(doc, id) {
                return Err(format!("{label}: holds {id}, which no package of it owns"));
            }
            if let Some(first) = self.origin.insert(id.clone(), label.to_string()) {
                return Err(format!("{label}: {id} is in {first} too"));
            }
            self.items.insert(id.clone(), v.clone());
        }
        Ok(())
    }
}
