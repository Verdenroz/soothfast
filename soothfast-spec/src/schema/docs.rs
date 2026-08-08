//! The rustdoc documents one resolution runs against.
//!
//! A crate's wire types routinely live in *other* crates of the same cargo
//! workspace. Those are absent from its own rustdoc `index` — identifiable
//! through `paths` but not walkable — which used to make every one of them a
//! reported gap and an open schema. Carrying the sibling crates' documents
//! alongside the package's own lets a type be walked out of whichever
//! document defines it, so a workspace-local type is as fully derived as a
//! local one.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// The rustdoc documents a resolution may walk: the package's own, plus zero
/// or more workspace-local crates its types can be defined in.
///
/// Auxiliary documents are consulted in the order given, and only after the
/// [`TypeTable`](super::TypeTable) — an explicit mapping is the documented
/// escape hatch for a type soothfast would otherwise get wrong, so it has to
/// keep winning over anything derived.
#[derive(Clone, Copy)]
pub struct Docs<'a> {
    primary: &'a Value,
    aux: &'a [Value],
}

impl<'a> Docs<'a> {
    /// One document, the way single-crate resolution has always worked.
    pub fn single(primary: &'a Value) -> Self {
        Self { primary, aux: &[] }
    }

    /// A package's document plus the workspace-local crates it reaches into.
    pub fn new(primary: &'a Value, aux: &'a [Value]) -> Self {
        Self { primary, aux }
    }

    /// The package's own rustdoc document.
    pub fn primary(self) -> &'a Value {
        self.primary
    }

    /// The auxiliary documents, in consultation order.
    pub fn aux(self) -> &'a [Value] {
        self.aux
    }
}

impl<'a> From<&'a Value> for Docs<'a> {
    fn from(primary: &'a Value) -> Self {
        Self::single(primary)
    }
}

/// One rustdoc JSON document, with the lookup tables cross-document
/// resolution needs.
pub(super) struct Doc<'a> {
    pub(super) index: &'a Map<String, Value>,
    pub(super) paths: &'a Map<String, Value>,
    /// This document's own crate, spelled the way rustdoc spells it
    /// (underscored). Empty when the document does not say.
    pub(super) krate: String,
    /// Canonical path → id, restricted to walkable struct/enum/alias items.
    by_path: BTreeMap<String, String>,
    /// Bare type name → id, `None` where the name is ambiguous in this crate.
    by_name: BTreeMap<String, Option<String>>,
}

impl<'a> Doc<'a> {
    /// Open a document, indexed by the canonical paths it can walk.
    ///
    /// Every document is indexed, not just the auxiliary ones: the primary
    /// document's own types take part in naming, and two same-named types in
    /// one crate have to be told apart exactly as two crates' are.
    pub(super) fn open(doc: &'a Value) -> Result<Self, String> {
        let index = doc["index"]
            .as_object()
            .ok_or("rustdoc JSON has no `index` object")?;
        let paths = doc["paths"]
            .as_object()
            .ok_or("rustdoc JSON has no `paths` object")?;
        let mut d = Self {
            index,
            paths,
            krate: crate_name(doc, paths),
            by_path: BTreeMap::new(),
            by_name: BTreeMap::new(),
        };
        for (id, entry) in paths {
            if !matches!(
                entry["kind"].as_str(),
                Some("struct" | "enum" | "type_alias" | "typedef")
            ) || !d.index.contains_key(id)
            {
                continue;
            }
            let Some(path) = joined_path(entry) else {
                continue;
            };
            let name = path.rsplit("::").next().unwrap_or(&path).to_string();
            d.by_path.entry(path).or_insert_with(|| id.clone());
            // A name claimed twice is ambiguous, and stays ambiguous.
            d.by_name
                .entry(name)
                .and_modify(|slot| *slot = None)
                .or_insert_with(|| Some(id.clone()));
        }
        Ok(d)
    }

    /// Every canonical path this document can actually walk, for naming.
    pub(super) fn walkable(&self) -> impl Iterator<Item = &String> {
        self.by_path.keys()
    }

    /// The item an id names in this document.
    pub(super) fn item(&self, id: &Value) -> Option<&'a Value> {
        self.index.get(&id_key(id)?)
    }

    /// Canonical definition path of an id, e.g. `core::time::Duration`.
    pub(super) fn canonical(&self, id: &Value) -> Option<String> {
        joined_path(self.paths.get(&id_key(id)?)?)
    }

    /// True when async-graphql, not serde, produces this type's JSON.
    ///
    /// Rustdoc drops the derive itself, so detection goes through the impls
    /// the derive generates: only a type async-graphql serves implements one
    /// of its container traits. `OutputType`/`InputType` are deliberately not
    /// in the set — a custom `#[Scalar]` implements those too, and its wire
    /// form is code, not fields.
    pub(super) fn serves_graphql(&self, item: &Value) -> bool {
        let inner = &item["inner"];
        let Some(impls) = inner
            .get("struct")
            .or_else(|| inner.get("enum"))
            .and_then(|k| k["impls"].as_array())
        else {
            return false;
        };
        impls
            .iter()
            .filter_map(|id| self.item(id))
            .filter_map(|im| self.canonical(&im["inner"]["impl"]["trait"]["id"]))
            .any(|path| is_graphql_container_trait(&path))
    }

    /// Locate a type this document defines, given the canonical path another
    /// document reported for it.
    ///
    /// Exact path match first. Failing that, a bare-name match — but only
    /// when the path names *this* crate and the name is unambiguous here, so
    /// a re-export whose two documents disagree on the module path resolves
    /// while two unrelated same-named types stay unresolved.
    pub(super) fn locate(&self, canonical: &str) -> Option<Value> {
        if let Some(id) = self.by_path.get(canonical) {
            return Some(Value::String(id.clone()));
        }
        let first = canonical.split("::").next()?;
        if self.krate.is_empty() || first != self.krate {
            return None;
        }
        self.locate_name(canonical.rsplit("::").next()?)
    }

    /// Locate a type by its plain name, when it is unambiguous here.
    pub(super) fn locate_name(&self, name: &str) -> Option<Value> {
        self.by_name
            .get(name)?
            .as_ref()
            .map(|id| Value::String(id.clone()))
    }

    /// Find a struct or enum by its plain name, for attribute overrides that
    /// name a type rather than pointing at a signature.
    pub(super) fn scan_type_id(&self, name: &str) -> Option<Value> {
        self.index
            .iter()
            .find(|(_, item)| {
                item["name"].as_str() == Some(name)
                    && (item["inner"].get("struct").is_some()
                        || item["inner"].get("enum").is_some())
            })
            .and_then(|(k, _)| {
                k.parse::<u64>()
                    .ok()
                    .map(Value::from)
                    .or_else(|| Some(Value::String(k.clone())))
            })
    }
}

/// Rustdoc ids appear as integers or strings depending on version.
fn id_key(id: &Value) -> Option<String> {
    match id {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Traits only a type async-graphql *serves* implements. Deliberately not
/// `OutputType`/`InputType`, which a custom `#[Scalar]` implements too.
fn is_graphql_container_trait(path: &str) -> bool {
    path.split("::").next() == Some("async_graphql")
        && matches!(
            path.rsplit("::").next(),
            Some(
                "ContainerType"
                    | "ObjectType"
                    | "InterfaceType"
                    | "UnionType"
                    | "InputObjectType"
                    | "OneofObjectType"
                    | "EnumType"
            )
        )
}

/// The `::`-joined path of a `paths` entry.
fn joined_path(entry: &Value) -> Option<String> {
    let segs: Vec<&str> = entry["path"]
        .as_array()?
        .iter()
        .filter_map(|s| s.as_str())
        .collect();
    Some(segs.join("::"))
}

/// The crate a document documents: the first segment of its root module's
/// path, falling back to any item the document owns.
fn crate_name(doc: &Value, paths: &Map<String, Value>) -> String {
    let root = doc
        .get("root")
        .and_then(id_key)
        .and_then(|r| paths.get(&r))
        .and_then(joined_path);
    if let Some(name) = root.as_deref().and_then(|p| p.split("::").next()) {
        if !name.is_empty() {
            return name.to_string();
        }
    }
    paths
        .values()
        .find(|e| e["crate_id"].as_u64() == Some(0))
        .and_then(joined_path)
        .and_then(|p| p.split("::").next().map(String::from))
        .unwrap_or_default()
}
