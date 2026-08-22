//! Rustdoc function items → [`ExportedFn`].
//!
//! A plain function has no extractor wrappers to classify: every parameter
//! is itself, so the only questions are how it takes its argument, what it
//! returns, and whether it can fail.

use serde_json::Value;

use crate::gap::Gap;
use crate::model::{ExportRecord, ExportedFn, Ownership, Param, Receiver, Ty};
use crate::resolve::{Resolver, generic_args};

/// Read one function item against the metadata its annotation recorded.
pub(crate) fn walk(r: &mut Resolver, item: &Value, record: &ExportRecord) -> ExportedFn {
    let at = record.id.clone();
    let function = &item["inner"]["function"];
    r.enter(record.owner.as_deref());

    for param in generic_params(function) {
        r.record(Gap::Generic {
            at: at.clone(),
            param,
        });
    }

    let mut receiver = Receiver::None;
    let mut params = Vec::new();
    for input in function["sig"]["inputs"].as_array().into_iter().flatten() {
        let name = input[0].as_str().unwrap_or_default();
        let ty = &input[1];
        if name == "self" {
            receiver = receiver_of(ty);
            continue;
        }
        params.push(Param {
            name: name.to_string(),
            ty: r.resolve(ty, &at),
            ownership: ownership_of(ty),
        });
    }

    let (ret, throws) = returns(r, &function["sig"]["output"], &at);
    let is_async = function["header"]["is_async"].as_bool().unwrap_or(false);

    if receiver == Receiver::Consuming {
        r.record(Gap::ConsumingReceiver { at: at.clone() });
    }
    if is_async && receiver == Receiver::Exclusive {
        r.record(Gap::AsyncExclusiveReceiver { at: at.clone() });
    }

    let name = item["name"].as_str().unwrap_or_default().to_string();
    ExportedFn {
        rust_path: record.id.clone(),
        id: record.id.clone(),
        name,
        owner: record.owner.clone(),
        receiver,
        params,
        ret,
        throws,
        is_async,
        constructor: record.constructor,
        doc: record.summary.clone(),
        skip: record.skip.clone(),
    }
}

/// The `Ok` and `Err` halves of the return type. A `Result` binds as a value
/// plus a raised error, never as a two-armed union.
fn returns(r: &mut Resolver, output: &Value, at: &str) -> (Ty, Option<Ty>) {
    if output.is_null() {
        return (Ty::Unit, None);
    }
    let path = output["resolved_path"]["path"].as_str().unwrap_or_default();
    if path == "Result" || path.ends_with("::Result") {
        let args = generic_args(&output["resolved_path"]);
        let ok = match args.first() {
            Some(ty) => r.resolve(ty, at),
            None => Ty::Unit,
        };
        let err = args.get(1).map(|ty| r.resolve_message(ty, at));
        return (ok, err);
    }
    (r.resolve(output, at), None)
}

fn receiver_of(ty: &Value) -> Receiver {
    match ty.get("borrowed_ref") {
        Some(r) if r["is_mutable"].as_bool().unwrap_or(false) => Receiver::Exclusive,
        Some(_) => Receiver::Shared,
        None => Receiver::Consuming,
    }
}

fn ownership_of(ty: &Value) -> Ownership {
    match ty.get("borrowed_ref") {
        Some(r) if r["is_mutable"].as_bool().unwrap_or(false) => Ownership::BorrowedMut,
        Some(_) => Ownership::Borrowed,
        None => Ownership::Owned,
    }
}

/// Type parameters the signature left open. Lifetimes bind fine and are not
/// reported.
fn generic_params(function: &Value) -> Vec<String> {
    function["generics"]["params"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["kind"].get("type").is_some())
        .filter_map(|p| p["name"].as_str().map(ToString::to_string))
        .collect()
}
