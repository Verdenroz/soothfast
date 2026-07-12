//! Compatibility diffing between two generated OpenAPI documents.
//!
//! Once a spec is generated it can no longer disagree with the code, so the
//! question worth gating shifts: not "does this match?" but "did this break
//! the people calling it?". That is a comparison against the merge-base, and
//! its answer is aimed at consumers rather than at the author.
//!
//! Requiredness means opposite things in the two directions, which is the
//! subtlety the whole comparison turns on (see [`crate::compat`]):
//!
//! - **Request** (client → server): the server growing *stricter* breaks
//!   callers. A newly required field is breaking; dropping a requirement is
//!   not.
//! - **Response** (server → client): the server providing *less* breaks
//!   callers. A field that stops being guaranteed is breaking; a new one is
//!   not.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::compat::{Change, Direction, SchemaDiff, Severity, sort};

/// Compare two generated documents, oldest first.
///
/// Returns every difference found, breaking ones first, then by location so
/// output is stable between runs.
pub fn diff(old: &Value, new: &Value) -> Vec<Change> {
    let mut changes = Vec::new();
    let old_ops = operations(old);
    let new_ops = operations(new);
    let schemas = SchemaDiff::new(old, new);

    let keys: BTreeSet<&String> = old_ops.keys().chain(new_ops.keys()).collect();
    for key in keys {
        match (old_ops.get(key), new_ops.get(key)) {
            (Some(_), None) => changes.push(Change::new(
                Severity::Breaking,
                key.clone(),
                "operation removed",
            )),
            (None, Some(_)) => changes.push(Change::new(
                Severity::Additive,
                key.clone(),
                "operation added",
            )),
            (Some(o), Some(n)) => compare_operation(&mut changes, key, &schemas, o, n),
            (None, None) => {}
        }
    }

    sort(&mut changes);
    changes
}

/// `"POST /items"` → operation object, for every operation in a document.
fn operations(doc: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let Some(paths) = doc["paths"].as_object() else {
        return out;
    };
    for (path, item) in paths {
        let Some(methods) = item.as_object() else {
            continue;
        };
        for (verb, op) in methods {
            out.insert(format!("{} {path}", verb.to_uppercase()), op.clone());
        }
    }
    out
}

fn compare_operation(
    changes: &mut Vec<Change>,
    at: &str,
    schemas: &SchemaDiff,
    old: &Value,
    new: &Value,
) {
    compare_parameters(changes, at, old, new);

    // Request body.
    match (body_schema(old), body_schema(new)) {
        (Some(o), Some(n)) => schemas.compare(
            changes,
            &format!("{at} requestBody"),
            &o,
            &n,
            Direction::Request,
        ),
        (Some(_), None) => {
            changes.push(Change::new(Severity::Breaking, at, "request body removed"))
        }
        // A body the server did not previously read is now expected.
        (None, Some(_)) => changes.push(Change::new(
            Severity::Breaking,
            at,
            "request body added — callers that sent none will fail",
        )),
        (None, None) => {}
    }

    // Responses, keyed by status code.
    let empty = serde_json::Map::new();
    let old_res = old["responses"].as_object().unwrap_or(&empty);
    let new_res = new["responses"].as_object().unwrap_or(&empty);
    let codes: BTreeSet<&String> = old_res.keys().chain(new_res.keys()).collect();
    for code in codes {
        match (old_res.get(code), new_res.get(code)) {
            (Some(_), None) => changes.push(Change::new(
                Severity::Breaking,
                format!("{at} {code}"),
                "response status removed",
            )),
            (None, Some(_)) => changes.push(Change::new(
                Severity::Additive,
                format!("{at} {code}"),
                "response status added",
            )),
            (Some(o), Some(n)) => {
                if let (Some(os), Some(ns)) = (content_schema(o), content_schema(n)) {
                    schemas.compare(
                        changes,
                        &format!("{at} {code}"),
                        &os,
                        &ns,
                        Direction::Response,
                    );
                }
            }
            (None, None) => {}
        }
    }
}

fn compare_parameters(changes: &mut Vec<Change>, at: &str, old: &Value, new: &Value) {
    let index = |op: &Value| -> BTreeMap<String, Value> {
        op["parameters"]
            .as_array()
            .map(|ps| {
                ps.iter()
                    .map(|p| {
                        let name = p["name"].as_str().unwrap_or("");
                        let loc = p["in"].as_str().unwrap_or("");
                        (format!("{loc}:{name}"), p.clone())
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let (old_p, new_p) = (index(old), index(new));
    let keys: BTreeSet<&String> = old_p.keys().chain(new_p.keys()).collect();

    for key in keys {
        let where_ = format!("{at} parameter {key}");
        match (old_p.get(key), new_p.get(key)) {
            (Some(o), None) => {
                let severity = if o["required"] == Value::Bool(true) {
                    Severity::Breaking
                } else {
                    Severity::Additive
                };
                changes.push(Change::new(severity, where_, "parameter removed"));
            }
            (None, Some(n)) => {
                // Only a newly *required* parameter can break a caller.
                let severity = if n["required"] == Value::Bool(true) {
                    Severity::Breaking
                } else {
                    Severity::Additive
                };
                changes.push(Change::new(severity, where_, "parameter added"));
            }
            (Some(o), Some(n)) => {
                match (
                    o["required"] == Value::Bool(true),
                    n["required"] == Value::Bool(true),
                ) {
                    (false, true) => changes.push(Change::new(
                        Severity::Breaking,
                        &where_,
                        "parameter became required",
                    )),
                    (true, false) => changes.push(Change::new(
                        Severity::Additive,
                        &where_,
                        "parameter became optional",
                    )),
                    _ => {}
                }
                let (ot, nt) = (&o["schema"]["type"], &n["schema"]["type"]);
                if ot != nt && !ot.is_null() && !nt.is_null() {
                    changes.push(Change::new(
                        Severity::Breaking,
                        &where_,
                        format!("type {ot} -> {nt}"),
                    ));
                }
            }
            (None, None) => {}
        }
    }
}

fn body_schema(op: &Value) -> Option<Value> {
    content_schema(&op["requestBody"])
}

/// The first (and in generated documents, only) content schema.
fn content_schema(holder: &Value) -> Option<Value> {
    holder["content"]
        .as_object()?
        .values()
        .next()
        .map(|c| c["schema"].clone())
        .filter(|s| !s.is_null())
}
