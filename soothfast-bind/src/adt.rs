//! Rustdoc struct and enum items → [`ExportedType`].
//!
//! Shape classification only. Nothing here reads serde attributes: a type
//! bound natively is handed across as itself, so how it would have
//! serialized says nothing about how it binds.

use serde_json::Value;

use crate::model::{ExportRecord, ExportedType, Field, Ty, TypeKind, Variant, VariantFields};
use crate::resolve::Resolver;

/// Read one struct or enum item against the metadata its annotation recorded.
pub(crate) fn walk(r: &mut Resolver, item: &Value, record: &ExportRecord) -> ExportedType {
    let at = record.id.clone();
    let name = item["name"].as_str().unwrap_or_default().to_string();
    r.enter(Some(&name));

    let inner = &item["inner"];
    let kind = if let Some(s) = inner.get("struct") {
        TypeKind::Struct(struct_fields(r, &s["kind"], &at))
    } else {
        TypeKind::Enum(variants(r, &inner["enum"]["variants"], &at))
    };

    ExportedType {
        rust_path: record.id.clone(),
        id: record.id.clone(),
        name,
        kind,
        send: auto_trait(r, inner, "Send").unwrap_or(true),
        sync: auto_trait(r, inner, "Sync") == Some(true),
        doc: record.summary.clone(),
        skip: record.skip.clone(),
    }
}

fn struct_fields(r: &mut Resolver, kind: &Value, at: &str) -> Vec<Field> {
    if let Some(plain) = kind.get("plain") {
        return named_fields(r, &plain["fields"], at);
    }
    if let Some(tuple) = kind.get("tuple").and_then(Value::as_array) {
        return positional_fields(r, tuple, at);
    }
    Vec::new()
}

fn named_fields(r: &mut Resolver, ids: &Value, at: &str) -> Vec<Field> {
    let mut out = Vec::new();
    for field in items(r, ids.as_array().unwrap_or(&Vec::new())) {
        let name = field["name"].as_str().unwrap_or_default().to_string();
        let where_ = format!("{at}.{name}");
        out.push(Field {
            ty: r.resolve(&field["inner"]["struct_field"], &where_),
            public: is_public(&field),
            doc: summary(&field),
            name,
        });
    }
    out
}

/// Tuple positions bind as fields named by index. A stripped (private) one
/// leaves a null id, which still counts toward the position.
fn positional_fields(r: &mut Resolver, ids: &[Value], at: &str) -> Vec<Field> {
    let mut out = Vec::new();
    for (n, field) in items(r, ids).into_iter().enumerate() {
        let where_ = format!("{at}.{n}");
        out.push(Field {
            name: n.to_string(),
            ty: r.resolve(&field["inner"]["struct_field"], &where_),
            public: is_public(&field),
            doc: summary(&field),
        });
    }
    out
}

fn variants(r: &mut Resolver, ids: &Value, at: &str) -> Vec<Variant> {
    let mut out = Vec::new();
    for variant in items(r, ids.as_array().unwrap_or(&Vec::new())) {
        let name = variant["name"].as_str().unwrap_or_default().to_string();
        let where_ = format!("{at}::{name}");
        out.push(Variant {
            fields: variant_fields(r, &variant["inner"]["variant"]["kind"], &where_),
            doc: summary(&variant),
            name,
        });
    }
    out
}

/// Resolve ids to owned items up front, so the walk can borrow the resolver
/// mutably while reading them.
fn items(r: &Resolver, ids: &[Value]) -> Vec<Value> {
    ids.iter().filter_map(|id| r.item(id).cloned()).collect()
}

fn variant_fields(r: &mut Resolver, kind: &Value, at: &str) -> VariantFields {
    if let Some(st) = kind.get("struct") {
        return VariantFields::Named(named_fields(r, &st["fields"], at));
    }
    if let Some(tuple) = kind.get("tuple").and_then(Value::as_array) {
        let mut tys: Vec<Ty> = Vec::new();
        for (n, field) in items(r, tuple).into_iter().enumerate() {
            tys.push(r.resolve(&field["inner"]["struct_field"], &format!("{at}.{n}")));
        }
        return VariantFields::Tuple(tys);
    }
    VariantFields::Unit
}

/// Whether the type carries one auto trait, or `None` when the document does
/// not say.
///
/// Auto-trait impls are only in the document when rustdoc was asked for them.
/// A caller that can afford a wrong guess defaults; one that cannot treats
/// the absence as a no.
fn auto_trait(r: &Resolver, inner: &Value, want: &str) -> Option<bool> {
    let impls = inner
        .get("struct")
        .or_else(|| inner.get("enum"))
        .and_then(|k| k["impls"].as_array())?;
    for im in impls.iter().filter_map(|id| r.item(id)) {
        let Some(path) = im["inner"]["impl"]["trait"]["path"].as_str() else {
            continue;
        };
        if path.rsplit("::").next().unwrap_or(path) != want {
            continue;
        }
        return Some(
            !im["inner"]["impl"]["is_negative"]
                .as_bool()
                .unwrap_or(false),
        );
    }
    None
}

fn is_public(item: &Value) -> bool {
    item["visibility"].as_str() != Some("default")
}

fn summary(item: &Value) -> Option<String> {
    let text = item["docs"].as_str()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}
