//! Consumer compatibility between two exported surfaces.
//!
//! An SDK's contract is its linked spec, so `sdk gate` gates the spec. A
//! native binding has none, so the surface is the contract and this compares
//! it directly.

use crate::model::Surface;

/// One difference a consumer of the generated package would notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Registry id of the item that moved.
    pub at: String,
    pub kind: ChangeKind,
}

/// What happened to an item between two surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// New item. Existing callers cannot be looking at it yet.
    Added,
    /// The item is gone. Every caller of it stops working.
    Removed,
    /// The bound signature moved under a name callers already use.
    Changed,
}

impl Change {
    /// Whether existing code stops working because of this.
    pub fn breaking(&self) -> bool {
        !matches!(self.kind, ChangeKind::Added)
    }

    /// One line naming the item and what happened to it.
    pub fn explain(&self) -> String {
        match self.kind {
            ChangeKind::Added => format!("added {}", self.at),
            ChangeKind::Removed => format!("removed {}", self.at),
            ChangeKind::Changed => format!("changed the bound signature of {}", self.at),
        }
    }
}

/// Compare two surfaces, oldest first.
///
/// Comparison runs over contract fingerprints, so a rewritten function body
/// is not a change while a moved parameter type is.
pub fn diff(base: &Surface, head: &Surface) -> Vec<Change> {
    let (before, after) = (base.fingerprints(), head.fingerprints());
    let mut changes = Vec::new();
    for (id, fingerprint) in &after {
        let kind = match before.get(id) {
            None => ChangeKind::Added,
            Some(old) if old != fingerprint => ChangeKind::Changed,
            Some(_) => continue,
        };
        changes.push(Change {
            at: id.clone(),
            kind,
        });
    }
    for id in before.keys().filter(|id| !after.contains_key(*id)) {
        changes.push(Change {
            at: id.clone(),
            kind: ChangeKind::Removed,
        });
    }
    changes.sort_by(|a, b| a.at.cmp(&b.at));
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ExportedFn, Receiver, Ty};

    fn func(name: &str, ret: Ty) -> ExportedFn {
        ExportedFn {
            id: format!("acme::{name}"),
            rust_path: format!("acme::{name}"),
            name: name.into(),
            owner: None,
            receiver: Receiver::None,
            params: Vec::new(),
            ret,
            throws: None,
            is_async: false,
            constructor: false,
            doc: None,
            skip: Vec::new(),
        }
    }

    fn surface(fns: Vec<ExportedFn>) -> Surface {
        Surface {
            fns,
            types: Vec::new(),
        }
    }

    #[test]
    fn an_unchanged_surface_reports_nothing() {
        let s = surface(vec![func("a", Ty::I64)]);
        assert!(diff(&s, &s).is_empty());
    }

    #[test]
    fn a_new_item_is_additive() {
        let base = surface(vec![func("a", Ty::I64)]);
        let head = surface(vec![func("a", Ty::I64), func("b", Ty::Bool)]);
        let changes = diff(&base, &head);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::Added);
        assert!(!changes[0].breaking());
    }

    #[test]
    fn a_dropped_item_breaks_its_callers() {
        let base = surface(vec![func("a", Ty::I64), func("b", Ty::Bool)]);
        let head = surface(vec![func("a", Ty::I64)]);
        let changes = diff(&base, &head);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].at, "acme::b");
        assert!(changes[0].breaking());
    }

    #[test]
    fn a_moved_return_type_breaks_under_a_name_callers_already_use() {
        let base = surface(vec![func("a", Ty::I64)]);
        let head = surface(vec![func("a", Ty::Str)]);
        let changes = diff(&base, &head);
        assert_eq!(changes[0].kind, ChangeKind::Changed);
        assert!(changes[0].breaking());
    }

    #[test]
    fn a_doc_only_edit_is_not_a_change() {
        let base = surface(vec![func("a", Ty::I64)]);
        let mut documented = func("a", Ty::I64);
        documented.doc = Some("now with prose".into());
        assert!(diff(&base, &surface(vec![documented])).is_empty());
    }
}
