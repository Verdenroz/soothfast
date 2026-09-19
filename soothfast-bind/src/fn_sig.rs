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
            ty: r.resolve_param(ty, &at),
            ownership: ownership_of(ty),
            inner_ownership: inner_ownership_of(ty),
        });
    }

    let (ret, throws) = returns(r, &function["sig"]["output"], &at);
    let output = &function["sig"]["output"];
    let ret_borrowed =
        ownership_of(output) != Ownership::Owned || inner_ownership_of(output) != Ownership::Owned;
    let is_async = function["header"]["is_async"].as_bool().unwrap_or(false);

    if receiver == Receiver::Consuming {
        r.record(Gap::ConsumingReceiver { at: at.clone() });
    }
    if is_async && receiver == Receiver::Exclusive {
        r.record(Gap::AsyncExclusiveReceiver { at: at.clone() });
    }

    let name = item["name"].as_str().unwrap_or_default().to_string();
    ExportedFn {
        rust_path: public_path(r, item, record).unwrap_or_else(|| record.id.clone()),
        id: record.id.clone(),
        name,
        owner: record.owner.clone(),
        receiver,
        params,
        ret,
        throws,
        ret_borrowed,
        is_async,
        constructor: record.constructor,
        doc: record.summary.clone(),
        skip: record.skip.clone(),
    }
}

/// The path a glue crate calls the function by: a free fn's own public
/// path, or a method's owner at its public path.
fn public_path(r: &Resolver, item: &Value, record: &ExportRecord) -> Option<String> {
    if record.owner.is_none() {
        return r.public_path(&item["id"]);
    }
    let (owner_path, name) = record.id.rsplit_once("::")?;
    let owner = r.find_by_path(owner_path)?;
    Some(format!("{}::{name}", r.public_path(&owner["id"])?))
}

/// The `Ok` and `Err` halves of the return type. A `Result` binds as a value
/// plus a raised error, never as a two-armed union.
fn returns(r: &mut Resolver, output: &Value, at: &str) -> (Ty, Option<Ty>) {
    if output.is_null() {
        return (Ty::Unit, None);
    }
    let mut output = output.clone();
    while let Some(target) = r.expand_alias(&output["resolved_path"]) {
        output = target;
    }
    let output = &output;
    let path = output["resolved_path"]["path"].as_str().unwrap_or_default();
    if path == "Result" || path.ends_with("::Result") {
        let args = generic_args(&output["resolved_path"]);
        // A real `Result<T, E>` always spells both type arguments; a single-
        // argument alias (`type Result<T> = std::result::Result<T, Error>`)
        // reads identically here but silently drops the error, so it is
        // reported rather than treated as an infallible `T`.
        if args.len() != 2 {
            r.record(Gap::UnmappedForeign {
                at: at.to_string(),
                path: path.to_string(),
            });
            return (Ty::Opaque(path.to_string()), None);
        }
        let ok = r.resolve(&args[0], at);
        let err = Some(r.resolve_message(&args[1], at));
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

/// Ownership of the type argument inside `Option<T>`. `Option<T>` is always
/// owned at its own top level, so this is the only place that distinguishes
/// `Option<&str>` from `Option<String>`.
fn inner_ownership_of(ty: &Value) -> Ownership {
    let path = ty["resolved_path"]["path"].as_str().unwrap_or_default();
    if path.rsplit("::").next() != Some("Option") {
        return Ownership::Owned;
    }
    match generic_args(&ty["resolved_path"]).first() {
        Some(arg) => ownership_of(arg),
        None => Ownership::Owned,
    }
}

/// Type parameters the signature left open. Lifetimes bind fine and are not
/// reported; neither is the synthetic parameter behind an `impl Trait`
/// argument, which [`Resolver::resolve_param`] decides on its own.
fn generic_params(function: &Value) -> Vec<String> {
    function["generics"]["params"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["kind"].get("type").is_some())
        .filter(|p| !p["kind"]["type"]["is_synthetic"].as_bool().unwrap_or(false))
        .filter_map(|p| p["name"].as_str().map(ToString::to_string))
        .collect()
}
