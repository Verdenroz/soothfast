//! Compatibility diffing between two generated GraphQL type graphs.
//!
//! GraphQL states nullability on the reference (`id: ID!`) rather than on the
//! parent, so this is not the JSON Schema comparison the other dialects
//! share — but the asymmetry underneath it is the same one. On an **output**
//! type, `T!` becoming `T` withdraws a guarantee a client already relies on;
//! on an **input** type, the same edit relaxes a demand, and it is `T`
//! becoming `T!` that breaks the caller.
//!
//! Comparing the graph rather than the SDL text is what lets the gate say
//! "field removed" instead of "line 42 changed": reformatting is invisible
//! here, and a real edit is named at the field it happened to.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::compat::{Change, Direction, Severity, compare_keys, sort};

/// Compare two generated type graphs, oldest first.
pub fn diff(old: &Value, new: &Value) -> Vec<Change> {
    let mut changes = Vec::new();

    compare_keys(
        &mut changes,
        &members(old, "types"),
        &members(new, "types"),
        "type",
        compare_type,
    );

    // Root fields are entry points: adding one can never break a caller, and
    // removing one always does, whichever root it sits on.
    for root in ["Query", "Mutation", "Subscription"] {
        let (o, n) = (fields(&old["roots"][root]), fields(&new["roots"][root]));
        let dir = Direction::Response;
        compare_keys(&mut changes, &o, &n, "field", |ch, at, o, n| {
            compare_field(ch, &format!("{root}.{at}"), o, n, dir);
            compare_arguments(ch, &format!("{root}.{at}"), o, n);
        });
    }

    sort(&mut changes);
    changes
}

fn members(doc: &Value, key: &str) -> BTreeMap<String, Value> {
    doc[key]
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

fn fields(decl: &Value) -> BTreeMap<String, Value> {
    decl.as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

fn compare_type(changes: &mut Vec<Change>, at: &str, old: &Value, new: &Value) {
    let (ok, nk) = (kind(old), kind(new));
    if ok != nk {
        changes.push(Change::new(
            Severity::Breaking,
            at,
            format!("kind {ok} -> {nk}"),
        ));
        return; // Nothing below the kind is comparable once it moved.
    }

    if ok == "enum" {
        compare_enum_values(changes, at, old, new);
        return;
    }
    if ok == "scalar" {
        return;
    }

    // An input type's fields flow inward; every other declaration's flow out.
    let dir = if ok == "input" {
        Direction::Request
    } else {
        Direction::Response
    };
    let (o, n) = (fields(&old["fields"]), fields(&new["fields"]));
    compare_field_set(changes, at, &o, &n, dir);
}

/// Field presence and shape, with requiredness read in the given direction.
fn compare_field_set(
    changes: &mut Vec<Change>,
    at: &str,
    old: &BTreeMap<String, Value>,
    new: &BTreeMap<String, Value>,
    dir: Direction,
) {
    let mut names: Vec<&String> = old.keys().chain(new.keys()).collect();
    names.sort_unstable();
    names.dedup();

    for name in names {
        let where_ = format!("{at}.{name}");
        match (old.get(name), new.get(name)) {
            (Some(_), None) => {
                changes.push(Change::new(Severity::Breaking, where_, "field removed"))
            }
            (None, Some(f)) => {
                // A new non-null input field is one every caller now has to
                // supply; a new output field is a gift.
                let severity = match (dir, non_null(type_of(f))) {
                    (Direction::Request, true) => Severity::Breaking,
                    _ => Severity::Additive,
                };
                changes.push(Change::new(severity, where_, "field added"));
            }
            (Some(o), Some(n)) => compare_field(changes, &where_, o, n, dir),
            (None, None) => {}
        }
    }
}

/// One field's type, compared in a known direction.
fn compare_field(changes: &mut Vec<Change>, at: &str, old: &Value, new: &Value, dir: Direction) {
    let (ot, nt) = (type_of(old), type_of(new));
    if ot == nt {
        return;
    }
    if shape(ot) != shape(nt) {
        changes.push(Change::new(
            Severity::Breaking,
            at,
            format!("type {ot} -> {nt}"),
        ));
        return;
    }
    // Same underlying type, different nullability.
    let tightened =
        non_null(nt) && !non_null(ot) || nt.matches('!').count() > ot.matches('!').count();
    let severity = match (dir, tightened) {
        // Demanding more of a caller, or guaranteeing less to one.
        (Direction::Request, true) | (Direction::Response, false) => Severity::Breaking,
        _ => Severity::Additive,
    };
    let detail = if tightened {
        format!("became non-null ({ot} -> {nt})")
    } else {
        format!("became nullable ({ot} -> {nt})")
    };
    changes.push(Change::new(severity, at, detail));
}

/// A field's arguments, which always flow inward however the field is used.
fn compare_arguments(changes: &mut Vec<Change>, at: &str, old: &Value, new: &Value) {
    let (o, n) = (fields(&old["args"]), fields(&new["args"]));
    if o.is_empty() && n.is_empty() {
        return;
    }
    let mut renamed = Vec::new();
    compare_field_set(&mut renamed, at, &o, &n, Direction::Request);
    // Reword so an argument does not report itself as a field.
    changes.extend(renamed.into_iter().map(|c| Change {
        detail: c.detail.replace("field ", "argument "),
        ..c
    }));
}

fn compare_enum_values(changes: &mut Vec<Change>, at: &str, old: &Value, new: &Value) {
    let values = |v: &Value| -> Vec<String> {
        v["values"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (o, n) = (values(old), values(new));
    for gone in o.iter().filter(|v| !n.contains(v)) {
        changes.push(Change::new(
            Severity::Breaking,
            at,
            format!("value {gone} no longer accepted"),
        ));
    }
    for added in n.iter().filter(|v| !o.contains(v)) {
        changes.push(Change::new(
            Severity::Additive,
            at,
            format!("value {added} added"),
        ));
    }
}

fn kind(decl: &Value) -> &str {
    decl["kind"].as_str().unwrap_or("type")
}

fn type_of(field: &Value) -> &str {
    field["type"].as_str().unwrap_or("")
}

/// A type reference with every `!` removed, so two references can be compared
/// for "same underlying type" independently of nullability.
fn shape(t: &str) -> String {
    t.replace('!', "")
}

fn non_null(t: &str) -> bool {
    t.ends_with('!')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::is_compatible;
    use serde_json::json;

    fn graph(types: Value, query_fields: Value) -> Value {
        json!({ "types": types, "roots": { "Query": query_fields } })
    }

    fn item(fields: Value) -> Value {
        json!({ "Item": { "kind": "type", "fields": fields } })
    }

    fn new_item(fields: Value) -> Value {
        json!({ "NewItemInput": { "kind": "input", "fields": fields } })
    }

    #[test]
    fn an_unchanged_graph_is_compatible_with_itself() {
        let g = graph(
            item(json!({ "id": { "type": "String!" } })),
            json!({ "item": { "type": "Item!" } }),
        );
        assert!(diff(&g, &g).is_empty());
    }

    #[test]
    fn removing_an_output_field_breaks_clients() {
        let old = graph(
            item(json!({ "id": { "type": "String!" }, "note": { "type": "String" } })),
            json!({}),
        );
        let new = graph(item(json!({ "id": { "type": "String!" } })), json!({}));
        let changes = diff(&old, &new);
        assert_eq!(changes.len(), 1, "got {changes:?}");
        assert_eq!(changes[0].severity, Severity::Breaking);
        assert_eq!(changes[0].at, "Item.note");
    }

    #[test]
    fn adding_an_output_field_is_additive() {
        let old = graph(item(json!({ "id": { "type": "String!" } })), json!({}));
        let new = graph(
            item(json!({ "id": { "type": "String!" }, "note": { "type": "String!" } })),
            json!({}),
        );
        assert!(is_compatible(&diff(&old, &new)));
    }

    #[test]
    fn an_output_field_losing_its_guarantee_breaks_clients() {
        let old = graph(item(json!({ "id": { "type": "String!" } })), json!({}));
        let new = graph(item(json!({ "id": { "type": "String" } })), json!({}));
        let changes = diff(&old, &new);
        assert_eq!(changes[0].severity, Severity::Breaking);
        assert!(changes[0].detail.contains("became nullable"), "{changes:?}");
    }

    #[test]
    fn the_same_edit_on_an_input_relaxes_a_demand() {
        let old = graph(
            new_item(json!({ "name": { "type": "String!" } })),
            json!({}),
        );
        let new = graph(new_item(json!({ "name": { "type": "String" } })), json!({}));
        let changes = diff(&old, &new);
        assert_eq!(changes[0].severity, Severity::Additive, "{changes:?}");
    }

    #[test]
    fn a_new_required_input_field_breaks_callers() {
        let old = graph(
            new_item(json!({ "name": { "type": "String!" } })),
            json!({}),
        );
        let new = graph(
            new_item(json!({ "name": { "type": "String!" }, "qty": { "type": "Int!" } })),
            json!({}),
        );
        let changes = diff(&old, &new);
        assert_eq!(changes[0].severity, Severity::Breaking);
        assert_eq!(changes[0].at, "NewItemInput.qty");
    }

    #[test]
    fn a_new_optional_input_field_does_not() {
        let old = graph(
            new_item(json!({ "name": { "type": "String!" } })),
            json!({}),
        );
        let new = graph(
            new_item(json!({ "name": { "type": "String!" }, "qty": { "type": "Int" } })),
            json!({}),
        );
        assert!(is_compatible(&diff(&old, &new)));
    }

    #[test]
    fn changing_the_underlying_type_is_breaking_either_way() {
        let old = graph(item(json!({ "id": { "type": "String!" } })), json!({}));
        let new = graph(item(json!({ "id": { "type": "Int!" } })), json!({}));
        let changes = diff(&old, &new);
        assert_eq!(changes[0].severity, Severity::Breaking);
        assert!(changes[0].detail.contains("String! -> Int!"));
    }

    #[test]
    fn list_item_nullability_is_read_as_a_guarantee_too() {
        let old = graph(item(json!({ "tags": { "type": "[String!]!" } })), json!({}));
        let new = graph(item(json!({ "tags": { "type": "[String]!" } })), json!({}));
        let changes = diff(&old, &new);
        assert_eq!(changes.len(), 1, "got {changes:?}");
        assert_eq!(changes[0].severity, Severity::Breaking);
    }

    #[test]
    fn removing_a_root_field_breaks_callers_and_adding_one_does_not() {
        let old = graph(json!({}), json!({ "item": { "type": "Item!" } }));
        let new = graph(json!({}), json!({ "other": { "type": "Item!" } }));
        let changes = diff(&old, &new);
        assert_eq!(changes.len(), 2, "got {changes:?}");
        assert_eq!(changes[0].severity, Severity::Breaking);
        assert_eq!(changes[0].at, "item");
    }

    #[test]
    fn a_newly_required_argument_breaks_callers() {
        let old = graph(
            json!({}),
            json!({ "item": { "type": "Item!", "args": { "id": { "type": "ID!" } } } }),
        );
        let new = graph(
            json!({}),
            json!({ "item": { "type": "Item!",
                              "args": { "id": { "type": "ID!" }, "tenant": { "type": "ID!" } } } }),
        );
        let changes = diff(&old, &new);
        assert_eq!(changes.len(), 1, "got {changes:?}");
        assert_eq!(changes[0].severity, Severity::Breaking);
        assert!(changes[0].detail.contains("argument added"), "{changes:?}");
    }

    #[test]
    fn an_optional_new_argument_is_additive() {
        let old = graph(json!({}), json!({ "item": { "type": "Item!" } }));
        let new = graph(
            json!({}),
            json!({ "item": { "type": "Item!", "args": { "tenant": { "type": "ID" } } } }),
        );
        assert!(is_compatible(&diff(&old, &new)), "{:?}", diff(&old, &new));
    }

    #[test]
    fn removing_an_enum_value_breaks_consumers() {
        let old = graph(
            json!({ "Status": { "kind": "enum", "values": ["ACTIVE", "ARCHIVED"] } }),
            json!({}),
        );
        let new = graph(
            json!({ "Status": { "kind": "enum", "values": ["ACTIVE"] } }),
            json!({}),
        );
        let changes = diff(&old, &new);
        assert_eq!(changes.len(), 1, "got {changes:?}");
        assert!(changes[0].detail.contains("ARCHIVED"));
        assert_eq!(changes[0].severity, Severity::Breaking);
    }

    #[test]
    fn a_type_that_becomes_an_input_is_breaking() {
        let old = graph(item(json!({ "id": { "type": "String!" } })), json!({}));
        let new = graph(
            json!({ "Item": { "kind": "input", "fields": { "id": { "type": "String!" } } } }),
            json!({}),
        );
        let changes = diff(&old, &new);
        assert_eq!(changes.len(), 1, "got {changes:?}");
        assert!(changes[0].detail.contains("kind type -> input"));
    }
}
