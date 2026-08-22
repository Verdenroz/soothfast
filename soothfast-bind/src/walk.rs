//! Registry records plus rustdoc JSON → the exported [`Surface`].
//!
//! The registry says what was exported; rustdoc says what shape it has.
//! Neither half alone is enough: the annotation is gone from rustdoc by the
//! time it runs, and the registry carries no types.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::foreign::TypeTable;
use crate::gap::Gap;
use crate::model::{ExportRecord, Surface};
use crate::resolve::Resolver;
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

    let exported: BTreeSet<String> = records
        .iter()
        .filter(|r| is_type(r))
        .map(|r| r.id.clone())
        .collect();
    let mut resolver = Resolver::new(doc, table, exported)?;
    let mut surface = Surface::default();

    for record in &records {
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
                    surface.fns.push(fn_sig::walk(&mut resolver, &item, record));
                }
                None => resolver.record(missing(record)),
            },
            "method" => match find_method(&resolver, &record.id) {
                Some(item) => {
                    let item = item.clone();
                    surface.fns.push(fn_sig::walk(&mut resolver, &item, record));
                }
                None => resolver.record(missing(record)),
            },
            _ => resolver.record(missing(record)),
        }
    }

    Ok((surface, resolver.gaps))
}

fn is_type(record: &ExportRecord) -> bool {
    matches!(record.kind.as_str(), "struct" | "enum")
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
