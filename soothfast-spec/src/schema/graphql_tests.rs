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
