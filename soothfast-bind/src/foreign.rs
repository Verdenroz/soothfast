//! Types the resolver cannot walk, and what they bind as.
//!
//! A type outside the package's own rustdoc index has no fields to read, so
//! it either has a mapping here or becomes a reported gap. An explicit
//! mapping always wins over anything derived: it is the documented escape
//! hatch for a type soothfast would otherwise get wrong.

use std::collections::BTreeMap;

use crate::model::Ty;

/// Canonical path → the type it binds as.
#[derive(Debug, Clone, Default)]
pub struct TypeTable {
    entries: BTreeMap<String, Ty>,
}

impl TypeTable {
    /// A table with the std types every language already has a spelling for.
    pub fn with_defaults() -> Self {
        let mut table = TypeTable::default();
        for path in ["std::path::PathBuf", "std::path::Path"] {
            table.insert(path, Ty::Str);
        }
        for path in ["std::ffi::OsString", "std::ffi::OsStr"] {
            table.insert(path, Ty::Str);
        }
        table
    }

    /// Add a mapping, replacing any entry for the same path.
    pub fn insert(&mut self, path: &str, ty: Ty) {
        self.entries.insert(path.to_string(), ty);
    }

    /// Look a type up by canonical path, then by bare name.
    ///
    /// Rustdoc spells a path as the source did, so a type reached through a
    /// re-export does not match the canonical spelling a mapping uses.
    pub fn lookup(&self, path: &str) -> Option<&Ty> {
        if let Some(ty) = self.entries.get(path) {
            return Some(ty);
        }
        let name = path.rsplit("::").next()?;
        self.entries
            .iter()
            .find(|(k, _)| k.rsplit("::").next() == Some(name))
            .map(|(_, ty)| ty)
    }

    /// Whether the table has no entries at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_mapping_wins_over_the_defaults() {
        let mut table = TypeTable::with_defaults();
        assert_eq!(table.lookup("std::path::PathBuf"), Some(&Ty::Str));
        table.insert("std::path::PathBuf", Ty::Bytes);
        assert_eq!(table.lookup("std::path::PathBuf"), Some(&Ty::Bytes));
    }

    #[test]
    fn a_bare_name_matches_when_the_full_path_does_not() {
        let mut table = TypeTable::default();
        table.insert("chrono::DateTime", Ty::Str);
        assert_eq!(table.lookup("chrono::offset::DateTime"), Some(&Ty::Str));
        assert_eq!(table.lookup("other::Thing"), None);
    }

    #[test]
    fn an_empty_table_maps_nothing() {
        assert!(TypeTable::default().is_empty());
        assert_eq!(TypeTable::default().lookup("std::path::PathBuf"), None);
    }
}
