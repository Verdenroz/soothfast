//! Compatibility diff tests.
//!
//! The cases that matter most are the ones where requiredness flips meaning
//! between request and response: an identical structural edit is breaking in
//! one direction and harmless in the other.

use serde_json::{Value, json};

use super::diff::diff;
use crate::compat::{Change, Severity, is_compatible};

/// A document with one operation, given request and response schemas.
fn doc(request: Option<Value>, response: Value) -> Value {
    let mut op = json!({
        "operationId": "createItem",
        "responses": {
            "200": { "description": "OK",
                     "content": { "application/json": { "schema": response } } }
        }
    });
    if let Some(r) = request {
        op["requestBody"] = json!({
            "required": true,
            "content": { "application/json": { "schema": r } },
        });
    }
    json!({ "openapi": "3.1.0", "paths": { "/items": { "post": op } } })
}

fn object(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

fn at(changes: &[Change], needle: &str) -> Vec<String> {
    changes
        .iter()
        .filter(|c| c.at.contains(needle))
        .map(|c| format!("{:?} {}", c.severity, c.detail))
        .collect()
}

#[test]
fn an_identical_document_has_no_changes() {
    let d = doc(
        Some(object(json!({ "a": { "type": "string" } }), &["a"])),
        json!({}),
    );
    assert!(diff(&d, &d).is_empty());
    assert!(is_compatible(&diff(&d, &d)));
}

#[test]
fn removing_an_operation_breaks_callers() {
    let old = doc(None, json!({}));
    let new = json!({ "openapi": "3.1.0", "paths": {} });
    let changes = diff(&old, &new);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].severity, Severity::Breaking);
    assert_eq!(changes[0].detail, "operation removed");
    assert!(!is_compatible(&changes));
}

#[test]
fn adding_an_operation_is_additive() {
    let old = json!({ "openapi": "3.1.0", "paths": {} });
    let new = doc(None, json!({}));
    let changes = diff(&old, &new);
    assert_eq!(changes[0].severity, Severity::Additive);
    assert!(is_compatible(&changes));
}

// --- the requiredness asymmetry ------------------------------------------

#[test]
fn a_newly_required_request_field_breaks_callers() {
    let old = doc(
        Some(object(json!({ "a": { "type": "string" } }), &["a"])),
        json!({}),
    );
    let new = doc(
        Some(object(
            json!({ "a": { "type": "string" }, "b": { "type": "string" } }),
            &["a", "b"],
        )),
        json!({}),
    );
    let changes = diff(&old, &new);
    assert_eq!(
        at(&changes, "requestBody.b"),
        vec!["Breaking property added"]
    );
    assert!(!is_compatible(&changes));
}

#[test]
fn a_newly_optional_request_field_is_additive() {
    let old = doc(
        Some(object(json!({ "a": { "type": "string" } }), &["a"])),
        json!({}),
    );
    let new = doc(
        Some(object(
            json!({ "a": { "type": "string" }, "b": { "type": "string" } }),
            &["a"],
        )),
        json!({}),
    );
    assert!(is_compatible(&diff(&old, &new)));
}

#[test]
fn a_new_response_field_is_additive_even_when_required() {
    // The mirror of the request case: callers reading the old shape are fine.
    let old = doc(None, object(json!({ "a": { "type": "string" } }), &["a"]));
    let new = doc(
        None,
        object(
            json!({ "a": { "type": "string" }, "b": { "type": "string" } }),
            &["a", "b"],
        ),
    );
    let changes = diff(&old, &new);
    assert!(is_compatible(&changes), "got {changes:?}");
}

#[test]
fn a_response_field_that_stops_being_guaranteed_breaks_readers() {
    let old = doc(None, object(json!({ "a": { "type": "string" } }), &["a"]));
    let new = doc(None, object(json!({ "a": { "type": "string" } }), &[]));
    let changes = diff(&old, &new);
    assert_eq!(at(&changes, ".a"), vec!["Breaking no longer guaranteed"]);
}

#[test]
fn a_request_field_that_stops_being_required_is_additive() {
    // Same structural edit as the previous test, opposite direction.
    let old = doc(
        Some(object(json!({ "a": { "type": "string" } }), &["a"])),
        json!({}),
    );
    let new = doc(
        Some(object(json!({ "a": { "type": "string" } }), &[])),
        json!({}),
    );
    let changes = diff(&old, &new);
    assert!(is_compatible(&changes), "got {changes:?}");
}

#[test]
fn an_optional_request_field_becoming_required_breaks_callers() {
    let old = doc(
        Some(object(json!({ "a": { "type": "string" } }), &[])),
        json!({}),
    );
    let new = doc(
        Some(object(json!({ "a": { "type": "string" } }), &["a"])),
        json!({}),
    );
    assert_eq!(
        at(&diff(&old, &new), ".a"),
        vec!["Breaking became required"]
    );
}

// --- types, properties, enums --------------------------------------------

#[test]
fn a_changed_type_is_breaking_in_either_direction() {
    let old = doc(None, object(json!({ "a": { "type": "string" } }), &[]));
    let new = doc(None, object(json!({ "a": { "type": "integer" } }), &[]));
    let changes = diff(&old, &new);
    assert_eq!(at(&changes, ".a"), vec!["Breaking type string -> integer"]);
}

#[test]
fn a_removed_property_is_breaking() {
    let old = doc(None, object(json!({ "a": { "type": "string" } }), &[]));
    let new = doc(None, object(json!({}), &[]));
    assert_eq!(
        at(&diff(&old, &new), ".a"),
        vec!["Breaking property removed"]
    );
}

#[test]
fn a_removed_enum_value_is_breaking_and_an_added_one_is_not() {
    let old = doc(
        None,
        object(
            json!({ "s": { "type": "string", "enum": ["a", "b"] } }),
            &[],
        ),
    );
    let new = doc(
        None,
        object(
            json!({ "s": { "type": "string", "enum": ["a", "c"] } }),
            &[],
        ),
    );
    let changes = at(&diff(&old, &new), ".s");
    assert!(changes.contains(&"Breaking value b no longer accepted".to_string()));
    assert!(changes.contains(&"Additive value c added".to_string()));
}

#[test]
fn array_element_types_are_compared() {
    let old = doc(
        None,
        json!({ "type": "array", "items": { "type": "string" } }),
    );
    let new = doc(
        None,
        json!({ "type": "array", "items": { "type": "integer" } }),
    );
    let changes = diff(&old, &new);
    assert!(!is_compatible(&changes), "got {changes:?}");
    assert!(changes[0].at.contains("[]"), "got {:?}", changes[0]);
}

// --- parameters -----------------------------------------------------------

fn doc_with_params(params: Value) -> Value {
    json!({ "openapi": "3.1.0", "paths": { "/items": { "get": {
        "operationId": "listItems",
        "parameters": params,
        "responses": { "200": { "description": "OK" } },
    }}}})
}

#[test]
fn a_new_required_parameter_breaks_callers() {
    let old = doc_with_params(json!([]));
    let new = doc_with_params(json!([
        { "name": "q", "in": "query", "required": true, "schema": { "type": "string" } }
    ]));
    let changes = diff(&old, &new);
    assert_eq!(at(&changes, "query:q"), vec!["Breaking parameter added"]);
}

#[test]
fn a_new_optional_parameter_is_additive() {
    let old = doc_with_params(json!([]));
    let new = doc_with_params(json!([
        { "name": "q", "in": "query", "required": false, "schema": { "type": "string" } }
    ]));
    assert!(is_compatible(&diff(&old, &new)));
}

#[test]
fn a_removed_required_parameter_is_breaking() {
    let old = doc_with_params(json!([
        { "name": "q", "in": "query", "required": true, "schema": { "type": "string" } }
    ]));
    let new = doc_with_params(json!([]));
    assert_eq!(
        at(&diff(&old, &new), "query:q"),
        vec!["Breaking parameter removed"]
    );
}

#[test]
fn the_same_name_in_two_locations_is_not_confused() {
    let old = doc_with_params(json!([
        { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }
    ]));
    let new = doc_with_params(json!([
        { "name": "id", "in": "query", "required": true, "schema": { "type": "string" } }
    ]));
    let changes = diff(&old, &new);
    assert_eq!(at(&changes, "path:id"), vec!["Breaking parameter removed"]);
    assert_eq!(at(&changes, "query:id"), vec!["Breaking parameter added"]);
}

// --- responses and refs ---------------------------------------------------

#[test]
fn a_removed_response_status_is_breaking() {
    let old = json!({ "openapi": "3.1.0", "paths": { "/i": { "get": { "responses": {
        "200": { "description": "OK" }, "404": { "description": "Not Found" } }}}}});
    let new = json!({ "openapi": "3.1.0", "paths": { "/i": { "get": { "responses": {
        "200": { "description": "OK" } }}}}});
    let changes = diff(&old, &new);
    assert_eq!(
        at(&changes, "404"),
        vec!["Breaking response status removed"]
    );
}

#[test]
fn refs_are_followed_into_each_documents_own_components() {
    let build = |ty: &str| {
        json!({
            "openapi": "3.1.0",
            "paths": { "/i": { "get": { "responses": { "200": {
                "description": "OK",
                "content": { "application/json": {
                    "schema": { "$ref": "#/components/schemas/Item" } } } } } } } },
            "components": { "schemas": { "Item": {
                "type": "object", "properties": { "id": { "type": ty } } } } },
        })
    };
    let changes = diff(&build("string"), &build("integer"));
    assert_eq!(at(&changes, ".id"), vec!["Breaking type string -> integer"]);
}

#[test]
fn self_referential_schemas_terminate() {
    let build = |ty: &str| {
        json!({
            "openapi": "3.1.0",
            "paths": { "/i": { "get": { "responses": { "200": {
                "description": "OK",
                "content": { "application/json": {
                    "schema": { "$ref": "#/components/schemas/Node" } } } } } } } },
            "components": { "schemas": { "Node": {
                "type": "object",
                "properties": {
                    "value": { "type": ty },
                    "next": { "$ref": "#/components/schemas/Node" },
                } } } },
        })
    };
    let changes = diff(&build("string"), &build("integer"));
    assert!(!is_compatible(&changes), "the real change is still found");
}

#[test]
fn breaking_changes_are_listed_before_additive_ones() {
    let old = doc(None, object(json!({ "a": { "type": "string" } }), &["a"]));
    let new = doc(
        None,
        object(
            json!({ "a": { "type": "integer" }, "z": { "type": "string" } }),
            &["a"],
        ),
    );
    let changes = diff(&old, &new);
    assert_eq!(changes[0].severity, Severity::Breaking);
    assert_eq!(
        changes.last().map(|c| c.severity),
        Some(Severity::Additive),
        "got {changes:?}"
    );
}
