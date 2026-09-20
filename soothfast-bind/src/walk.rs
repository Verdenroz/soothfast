//! Registry records plus rustdoc JSON → the exported [`Surface`].
//!
//! The registry says what was exported; rustdoc says what shape it has.
//! Neither half alone is enough: the annotation is gone from rustdoc by the
//! time it runs, and the registry carries no types.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::foreign::TypeTable;
use crate::gap::Gap;
use crate::model::{ExportRecord, ExportedFn, Surface, Ty};
use crate::resolve::{Resolver, class_name};
use crate::{adt, fn_sig};

/// Read every exported item's shape out of the package's rustdoc document.
///
/// Records are walked in id order, so the surface does not depend on the
/// link order the registry slice happened to have.
pub fn surface(
    doc: &Value,
    table: &TypeTable,
    records: &[ExportRecord],
) -> Result<(Surface, Vec<Gap>), String> {
    let mut records = records.to_vec();
    records.sort_by(|a, b| a.id.cmp(&b.id));

    let mut exported: BTreeSet<String> = records
        .iter()
        .filter(|r| is_type(r))
        .map(|r| r.id.clone())
        .collect();
    let mut resolver = Resolver::new(doc, table, exported.clone())?;

    let excluded = excluded_types(&resolver, &records);
    for id in excluded.keys() {
        exported.remove(id);
        resolver.exclude(id);
    }

    let mut surface = Surface::default();

    for record in &records {
        if let Some(gap) = excluded.get(&record.id) {
            resolver.record(gap.clone());
            continue;
        }
        match record.kind.as_str() {
            "struct" | "enum" => match resolver.find_by_path(&record.id) {
                Some(item) => {
                    let item = item.clone();
                    surface.types.push(adt::walk(&mut resolver, &item, record));
                }
                None => resolver.record(missing(record)),
            },
            "fn" => match resolver.find_by_path(&record.id) {
                Some(item) => {
                    let item = item.clone();
                    let function = fn_sig::walk(&mut resolver, &item, record);
                    note_error(&mut resolver, &mut surface, &exported, &function);
                    surface.fns.push(function);
                }
                None => resolver.record(missing(record)),
            },
            "method" => match find_method(&resolver, &record.id) {
                Some(item) => {
                    let item = item.clone();
                    let function = fn_sig::walk(&mut resolver, &item, record);
                    note_error(&mut resolver, &mut surface, &exported, &function);
                    surface.fns.push(function);
                }
                None => resolver.record(missing(record)),
            },
            _ => resolver.record(missing(record)),
        }
    }

    Ok((surface, resolver.gaps))
}

/// Exported types that cannot become a class, keyed by id: a generic one
/// (nothing says which instantiation to bind), one with no public path at
/// all (defined in a private module, nothing re-exports it), or one whose
/// short name lost a collision to another exported type's.
/// [`ExportedType::name`](crate::model::ExportedType) is the last segment of
/// a type's public path, so two types under different modules (or one
/// aliased at the root) can end up wanting the same one; the shortest
/// public path keeps it, ties broken lexically, since two classes under one
/// name would not compile any more than a class naming itself through a
/// path nothing outside the crate can reach would.
fn excluded_types(resolver: &Resolver, records: &[ExportRecord]) -> BTreeMap<String, Gap> {
    let mut by_name: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut excluded = BTreeMap::new();
    for record in records.iter().filter(|r| is_type(r)) {
        let Some(item) = resolver.find_by_path(&record.id) else {
            continue;
        };
        if let Some(param) = type_generic_params(item).into_iter().next() {
            excluded.insert(
                record.id.clone(),
                Gap::Generic {
                    at: record.id.clone(),
                    param,
                },
            );
            continue;
        }
        match resolver.public_path(&item["id"]) {
            Some(rust_path) => {
                let name = class_name(&rust_path).to_string();
                by_name
                    .entry(name)
                    .or_default()
                    .push((rust_path, record.id.clone()));
            }
            None if resolver.has_module_tree() => {
                excluded.insert(
                    record.id.clone(),
                    Gap::Unreachable {
                        at: record.id.clone(),
                    },
                );
            }
            // A synthetic document with no module tree at all: every path
            // falls back to the item's own declaration path, as before.
            None => {
                let rust_path = record.id.clone();
                let name = class_name(&rust_path).to_string();
                by_name
                    .entry(name)
                    .or_default()
                    .push((rust_path, record.id.clone()));
            }
        }
    }

    for (name, mut group) in by_name {
        if group.len() < 2 {
            continue;
        }
        group.sort_by(|a, b| (a.0.len(), &a.0).cmp(&(b.0.len(), &b.0)));
        let winner = group[0].0.clone();
        for (path, id) in group.into_iter().skip(1) {
            excluded.insert(
                id,
                Gap::NameCollision {
                    at: path,
                    name: name.clone(),
                    winner: winner.clone(),
                },
            );
        }
    }
    excluded
}

/// The error type a call throws, read once per distinct type. A `String`
/// error has no shape to read, and a type outside the document (another
/// crate's) none the document can give.
fn note_error(
    resolver: &mut Resolver,
    surface: &mut Surface,
    exported: &BTreeSet<String>,
    function: &ExportedFn,
) {
    let canonical = match &function.throws {
        Some(Ty::Opaque(path)) => path.clone(),
        Some(Ty::Class(name)) => match exported
            .iter()
            .find(|id| id.rsplit("::").next() == Some(name))
        {
            Some(id) => id.clone(),
            None => return,
        },
        _ => return,
    };
    let Some(item) = resolver.find_by_path(&canonical) else {
        return;
    };
    let item = item.clone();
    let rust_path = resolver.public_path(&item["id"]).unwrap_or(canonical);
    if surface.errors.iter().any(|e| e.rust_path == rust_path) {
        return;
    }
    surface
        .errors
        .push(adt::error_type(resolver, &item, rust_path));
}

fn is_type(record: &ExportRecord) -> bool {
    matches!(record.kind.as_str(), "struct" | "enum")
}

/// Type parameters a struct or enum declares. A binding names a type by a
/// single Rust path, which a generic type has none of on its own.
fn type_generic_params(item: &Value) -> Vec<String> {
    item["inner"]
        .get("struct")
        .or_else(|| item["inner"].get("enum"))
        .and_then(|k| k["generics"]["params"].as_array())
        .into_iter()
        .flatten()
        .filter(|p| p["kind"].get("type").is_some())
        .filter_map(|p| p["name"].as_str().map(ToString::to_string))
        .collect()
}

/// An exported item rustdoc has no entry for, reported rather than dropped.
fn missing(record: &ExportRecord) -> Gap {
    Gap::UnmappedForeign {
        at: record.id.clone(),
        path: record.id.clone(),
    }
}

/// Locate a method by its registry id.
///
/// Methods are absent from rustdoc's `paths`, reachable only through the
/// impl blocks of the type that owns them.
fn find_method<'a>(r: &Resolver<'a>, id: &str) -> Option<&'a Value> {
    let (owner_path, name) = id.rsplit_once("::")?;
    let owner = r.find_by_path(owner_path)?;
    let inner = &owner["inner"];
    let impls = inner
        .get("struct")
        .or_else(|| inner.get("enum"))?
        .get("impls")?
        .as_array()?;
    impls
        .iter()
        .filter_map(|impl_id| r.item(impl_id))
        .filter(|im| im["inner"]["impl"]["trait"].is_null())
        .find_map(|im| {
            im["inner"]["impl"]["items"]
                .as_array()?
                .iter()
                .filter_map(|fid| r.item(fid))
                .find(|f| f["name"].as_str() == Some(name) && f["inner"].get("function").is_some())
        })
}
