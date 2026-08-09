//! Extraction over types async-graphql serves, where it — not serde — names
//! the wire.
//!
//! The fixture shapes here were pinned against real rustdoc JSON from a
//! nightly probe crate built on async-graphql 7.2: rustdoc drops the derive
//! but keeps `#[graphql(...)]` verbatim, and records the derive's generated
//! trait impls, which is what detection reads.

use serde_json::{Value, json};

use super::{Gap, TypeTable, extract_named};

/// Ids above the fixture's own range, for the generated impl items.
const IMPL_ID: u64 = 90;
const TRAIT_ID: u64 = 91;
/// Ids for the `#[ComplexObject]`-generated inherent impl fixtures below,
/// disjoint from the trait-impl ids above and the field id range.
const COMPLEX_IMPL_ID: u64 = 92;
const METHOD_ID: u64 = 93;

fn attrs_of(attrs: &[&str]) -> Vec<Value> {
    attrs.iter().map(|a| json!({ "other": a })).collect()
}

fn field(name: &str, attrs: &[&str]) -> Value {
    json!({ "name": name, "docs": Value::Null, "attrs": attrs_of(attrs),
            "inner": { "struct_field": { "primitive": "f64" } } })
}

/// A one-type document. `graphql` decides whether the type carries the trait
/// impl async-graphql's derive generates.
fn struct_doc(container: &[&str], fields: &[(&str, &[&str])], graphql: bool) -> Value {
    let ids: Vec<u64> = (0..fields.len()).map(|i| i as u64 + 2).collect();
    let impls: Vec<u64> = if graphql { vec![IMPL_ID] } else { vec![] };
    let mut index = vec![(
        1,
        json!({
            "name": "Item", "docs": Value::Null, "attrs": attrs_of(container),
            "inner": { "struct": {
                "kind": { "plain": { "fields": ids, "has_stripped_fields": false } },
                "generics": { "params": [], "where_predicates": [] },
                "impls": impls } },
        }),
    )];
    for (i, (name, attrs)) in fields.iter().enumerate() {
        index.push((i as u64 + 2, field(name, attrs)));
    }
    doc(index)
}

/// A C-like enum, optionally async-graphql-served.
fn enum_doc(container: &[&str], variants: &[(&str, &[&str])], graphql: bool) -> Value {
    let ids: Vec<u64> = (0..variants.len()).map(|i| i as u64 + 2).collect();
    let impls: Vec<u64> = if graphql { vec![IMPL_ID] } else { vec![] };
    let mut index = vec![(
        1,
        json!({
            "name": "Item", "docs": Value::Null, "attrs": attrs_of(container),
            "inner": { "enum": {
                "variants": ids,
                "generics": { "params": [], "where_predicates": [] },
                "impls": impls } },
        }),
    )];
    for (i, (name, attrs)) in variants.iter().enumerate() {
        index.push((
            i as u64 + 2,
            json!({ "name": name, "docs": Value::Null, "attrs": attrs_of(attrs),
                    "inner": { "variant": { "kind": "plain" } } }),
        ));
    }
    doc(index)
}

/// Wrap index entries in a document, always carrying the async-graphql trait
/// an impl can point at — an unreferenced `paths` entry changes nothing.
fn doc(index: Vec<(u64, Value)>) -> Value {
    let mut index: serde_json::Map<String, Value> = index
        .into_iter()
        .map(|(id, item)| (id.to_string(), item))
        .collect();
    index.insert(
        IMPL_ID.to_string(),
        json!({ "name": Value::Null, "docs": Value::Null, "attrs": [],
                "inner": { "impl": { "trait": { "path": "ObjectType", "id": TRAIT_ID } } } }),
    );
    let paths = json!({
        TRAIT_ID.to_string(): {
            "crate_id": 2, "kind": "trait",
            "path": ["async_graphql", "base", "ObjectType"] },
    });
    json!({ "index": index, "paths": paths })
}

/// A path type with optional generic arguments, mirroring `extract_tests`'s
/// helper of the same shape.
fn path(name: &str, id: u64, args: &[Value]) -> Value {
    let args = if args.is_empty() {
        Value::Null
    } else {
        json!({ "angle_bracketed": {
            "args": args.iter().map(|a| json!({ "type": a })).collect::<Vec<_>>(),
            "constraints": [] } })
    };
    json!({ "resolved_path": { "path": name, "id": id, "args": args } })
}

/// A struct with one plain (kept) field and one `#[graphql(skip)]`ed field
/// whose replacement is a `#[ComplexObject]` inherent-impl resolver method
/// of the same name — the shape `#[derive(SimpleObject, complex)]` +
/// `#[ComplexObject]` produces. `extra_inputs` are the resolver's own
/// GraphQL arguments (after the always-present `self` and macro-injected
/// `Context`); `output` is its return type node.
fn struct_doc_with_complex_field(
    skipped_name: &str,
    extra_inputs: &[(&str, Value)],
    output: Value,
) -> Value {
    const KEPT_FIELD_ID: u64 = 2;
    const SKIPPED_FIELD_ID: u64 = 3;
    let mut inputs = vec![
        json!(["self", { "borrowed_ref": { "type": { "generic": "Self" } } }]),
        json!(["_", { "borrowed_ref": {
            "type": { "resolved_path": { "path": "Context", "id": 40, "args": Value::Null } } } }]),
    ];
    inputs.extend(extra_inputs.iter().map(|(n, t)| json!([n, t])));

    let index = vec![
        (
            1,
            json!({
                "name": "Item", "docs": Value::Null, "attrs": [],
                "inner": { "struct": {
                    "kind": { "plain": { "fields": [KEPT_FIELD_ID, SKIPPED_FIELD_ID], "has_stripped_fields": false } },
                    "generics": { "params": [], "where_predicates": [] },
                    "impls": [IMPL_ID, COMPLEX_IMPL_ID] } },
            }),
        ),
        (KEPT_FIELD_ID, field("kept", &[])),
        (
            SKIPPED_FIELD_ID,
            json!({ "name": skipped_name, "docs": Value::Null,
                    "attrs": attrs_of(&["#[graphql(skip)]"]),
                    "inner": { "struct_field": { "primitive": "f64" } } }),
        ),
        (
            COMPLEX_IMPL_ID,
            json!({ "name": Value::Null, "docs": Value::Null, "attrs": [],
                    "inner": { "impl": { "trait": Value::Null, "items": [METHOD_ID] } } }),
        ),
        (
            METHOD_ID,
            json!({ "name": skipped_name, "docs": Value::Null, "attrs": [],
                    "inner": { "function": { "sig": { "inputs": inputs, "output": output } } } }),
        ),
    ];
    doc(index)
}

fn properties(d: &Value) -> Value {
    let e = extract_named(d, &TypeTable::builtin(), "Item").expect("extracts");
    e.components["Item"]["properties"].clone()
}

fn keys(d: &Value) -> Vec<String> {
    properties(d)
        .as_object()
        .expect("object schema")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn container_rename_fields_names_the_wire() {
    let d = struct_doc(
        &["#[graphql(rename_fields = \"camelCase\")]"],
        &[
            ("report_date", &[]),
            ("price_change_percentage_24h", &[]),
            ("market_cap_change_percentage_24h_usd", &[]),
        ],
        true,
    );
    assert_eq!(
        keys(&d),
        vec![
            "marketCapChangePercentage24HUsd",
            "priceChangePercentage24H",
            "reportDate"
        ]
    );
}

#[test]
fn a_served_type_with_no_graphql_attribute_is_still_camel_cased() {
    // async-graphql's default field rule is camelCase, so detection cannot
    // lean on the attribute being there.
    let d = struct_doc(&[], &[("report_date", &[]), ("open_interest", &[])], true);
    assert_eq!(keys(&d), vec!["openInterest", "reportDate"]);
}

#[test]
fn a_field_level_name_overrides_the_container_rule() {
    let d = struct_doc(
        &["#[graphql(rename_fields = \"camelCase\")]"],
        &[
            ("renamed_field", &["#[graphql(name = \"customName\")]"]),
            ("plain_field", &[]),
        ],
        true,
    );
    assert_eq!(keys(&d), vec!["customName", "plainField"]);
}

#[test]
fn graphql_naming_beats_serde_naming_on_a_served_type() {
    // The real case: `k`/`d` carry serde renames for parsing the *library's*
    // JSON, while async-graphql puts them on the wire as `k`/`d`.
    let d = struct_doc(
        &[
            "#[graphql(rename_fields = \"camelCase\")]",
            "#[serde(rename_all = \"SCREAMING_SNAKE_CASE\", default)]",
        ],
        &[
            ("k", &["#[serde(rename = \"%K\")]"]),
            ("d", &["#[serde(rename = \"%D\")]"]),
        ],
        true,
    );
    assert_eq!(keys(&d), vec!["d", "k"]);
}

#[test]
fn a_non_graphql_type_keeps_serdes_camel_case_exactly() {
    // Same input, no async-graphql: serde does *not* capitalise `24h`, and
    // that difference is the whole reason the two cannot share one rule.
    let d = struct_doc(
        &["#[serde(rename_all = \"camelCase\")]"],
        &[("price_change_percentage_24h", &[]), ("report_date", &[])],
        false,
    );
    assert_eq!(
        keys(&d),
        vec!["priceChangePercentage24h", "reportDate"],
        "serde's rule is untouched"
    );
}

#[test]
fn a_non_graphql_type_ignores_stray_graphql_field_attributes() {
    let d = struct_doc(
        &["#[serde(rename_all = \"camelCase\")]"],
        &[("report_date", &["#[graphql(name = \"nope\")]"])],
        false,
    );
    assert_eq!(keys(&d), vec!["reportDate"]);
}

#[test]
fn graphql_skip_takes_a_field_off_the_wire_and_serde_skip_does_not() {
    let d = struct_doc(
        &[],
        &[
            ("kept", &["#[serde(skip)]"]),
            ("dropped", &["#[graphql(skip)]"]),
        ],
        true,
    );
    assert_eq!(
        keys(&d),
        vec!["kept"],
        "serde does not produce this type's JSON, so its skip is not the wire's"
    );
}

#[test]
fn an_unknown_graphql_rename_rule_is_a_gap_not_a_guess() {
    // kebab-case is a serde rule; async-graphql has no such thing.
    let d = struct_doc(
        &["#[graphql(rename_fields = \"kebab-case\")]"],
        &[("report_date", &[])],
        true,
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    assert_eq!(
        e.gaps,
        vec![Gap::UnknownRenameRule {
            at: "Item".into(),
            rule: "kebab-case".into(),
        }]
    );
    assert!(
        e.components["Item"]["properties"]
            .get("reportDate")
            .is_some(),
        "falls back to async-graphql's default rather than serde's"
    );
}

#[test]
fn a_served_type_reports_no_gap_for_serdes_own_unknown_rule() {
    let d = struct_doc(
        &["#[serde(rename_all = \"Train-Case\")]"],
        &[("report_date", &[])],
        true,
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    assert!(
        e.gaps.is_empty(),
        "serde's rule never reaches the wire here"
    );
}

#[test]
fn detection_falls_back_to_the_attribute_when_impls_are_absent() {
    let d = struct_doc(
        &["#[graphql(rename_fields = \"camelCase\")]"],
        &[("price_change_percentage_24h", &[])],
        false,
    );
    assert_eq!(keys(&d), vec!["priceChangePercentage24H"]);
}

#[test]
fn enum_items_default_to_screaming_snake_case() {
    let d = enum_doc(&[], &[("NotFound", &[]), ("OkThen", &[])], true);
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    assert_eq!(
        e.components["Item"]["enum"],
        json!(["NOT_FOUND", "OK_THEN"])
    );
}

#[test]
fn rename_items_is_a_separate_rule_from_rename_fields() {
    let d = enum_doc(
        &["#[graphql(rename_fields = \"camelCase\", rename_items = \"camelCase\")]"],
        &[
            ("NotFound", &[]),
            ("Weird", &["#[graphql(name = \"WEIRD\")]"]),
        ],
        true,
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    assert_eq!(e.components["Item"]["enum"], json!(["notFound", "WEIRD"]));
}

#[test]
fn a_non_graphql_enum_keeps_serde_variant_naming() {
    let d = enum_doc(
        &["#[serde(rename_all = \"kebab-case\")]"],
        &[
            ("NotFound", &[]),
            ("OkThen", &["#[serde(rename = \"boom\")]"]),
        ],
        false,
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    assert_eq!(e.components["Item"]["enum"], json!(["not-found", "boom"]));
}

#[test]
fn a_graphql_skip_field_is_recovered_from_its_complex_object_resolver() {
    // The `GqlDividendsBatch` shape: a stored field skipped off the wire,
    // replaced by a `#[ComplexObject]` method of the same name returning
    // `Result<T, Error>` with one optional argument.
    let output = json!({ "resolved_path": { "path": "Result", "id": 60, "args": {
        "angle_bracketed": { "args": [
            { "type": { "primitive": "u32" } },
            { "type": { "resolved_path": { "path": "Error", "id": 61, "args": Value::Null } } },
        ], "constraints": [] } } } });
    let d = struct_doc_with_complex_field(
        "dividends",
        &[(
            "first",
            path("Option", 50, &[json!({ "primitive": "i32" })]),
        )],
        output,
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    assert!(e.gaps.is_empty(), "got {:?}", e.gaps);
    let props = &e.components["Item"]["properties"];
    assert_eq!(props["dividends"]["type"], "integer", "got {props:?}");
    assert!(props.get("kept").is_some());
    // Result<T, E> (not Result<Option<T>, E>) is a required field.
    assert_eq!(
        e.components["Item"]["required"],
        json!(["kept", "dividends"])
    );
}

#[test]
fn the_recovered_fields_wire_name_follows_the_containers_rename_rule() {
    let output = json!({ "primitive": "u32" });
    let d = struct_doc_with_complex_field("price_history", &[], output);
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    let props = &e.components["Item"]["properties"];
    assert!(
        props.get("priceHistory").is_some(),
        "the method's Rust name goes through the same camelCase rule a plain field would: {props:?}"
    );
}

#[test]
fn a_required_resolver_argument_is_a_gap_not_a_guess() {
    let output = json!({ "primitive": "u32" });
    let d = struct_doc_with_complex_field(
        "dividends",
        &[("required_arg", json!({ "primitive": "str" }))],
        output,
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    assert_eq!(
        e.gaps,
        vec![Gap::ComplexFieldArgument {
            at: "Item.dividends".into(),
            argument: "required_arg".into(),
        }]
    );
    assert!(
        e.components["Item"]["properties"]
            .get("dividends")
            .is_none(),
        "an unresolvable resolver drops the field rather than guessing its shape"
    );
}
