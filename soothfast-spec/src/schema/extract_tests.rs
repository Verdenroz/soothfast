//! End-to-end extraction tests over synthetic rustdoc JSON.
//!
//! Fixtures are hand-built rather than captured from a real `cargo rustdoc`
//! run: the shapes are pinned against a live nightly probe, and synthetic
//! input keeps the suite off the nightly toolchain and stable across rustdoc
//! releases.

use serde_json::{Value, json};

use super::{Gap, TypeTable, extract_named};

/// A rustdoc document from index entries keyed by id, plus foreign paths.
fn doc(index: Vec<(u64, Value)>, paths: Vec<(u64, &str)>) -> Value {
    let index: serde_json::Map<String, Value> = index
        .into_iter()
        .map(|(id, item)| (id.to_string(), item))
        .collect();
    let paths: serde_json::Map<String, Value> = paths
        .into_iter()
        .map(|(id, path)| {
            let segs: Vec<&str> = path.split("::").collect();
            (
                id.to_string(),
                json!({ "crate_id": 1, "path": segs, "kind": "struct" }),
            )
        })
        .collect();
    json!({ "index": index, "paths": paths })
}

fn struct_item(name: &str, fields: &[u64], attrs: &[&str]) -> Value {
    json!({
        "name": name, "docs": Value::Null, "attrs": attrs_of(attrs),
        "inner": { "struct": {
            "kind": { "plain": { "fields": fields, "has_stripped_fields": false } },
            "generics": { "params": [], "where_predicates": [] },
        }},
    })
}

fn generic_struct(name: &str, params: &[&str], fields: &[u64], attrs: &[&str]) -> Value {
    let params: Vec<Value> = params
        .iter()
        .map(|p| json!({ "name": p, "kind": { "type": {} } }))
        .collect();
    json!({
        "name": name, "docs": Value::Null, "attrs": attrs_of(attrs),
        "inner": { "struct": {
            "kind": { "plain": { "fields": fields, "has_stripped_fields": false } },
            "generics": { "params": params, "where_predicates": [] },
        }},
    })
}

fn field(name: &str, ty: Value, attrs: &[&str]) -> Value {
    json!({ "name": name, "docs": Value::Null, "attrs": attrs_of(attrs),
            "inner": { "struct_field": ty } })
}

fn attrs_of(attrs: &[&str]) -> Vec<Value> {
    attrs.iter().map(|a| json!({ "other": a })).collect()
}

fn prim(p: &str) -> Value {
    json!({ "primitive": p })
}

/// A path type with optional generic arguments.
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

#[test]
fn renames_fields_under_the_container_rule() {
    let d = doc(
        vec![
            (
                1,
                struct_item("Item", &[2, 3], &["#[serde(rename_all = \"camelCase\")]"]),
            ),
            (2, field("item_id", prim("u32"), &[])),
            (3, field("unit_price", prim("f64"), &[])),
        ],
        vec![],
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    let props = &e.components["Item"]["properties"];
    assert!(props.get("itemId").is_some(), "got {props:?}");
    assert!(props.get("unitPrice").is_some());
    assert!(e.gaps.is_empty());
}

#[test]
fn explicit_rename_and_skip_are_honoured() {
    let d = doc(
        vec![
            (1, struct_item("Item", &[2, 3], &[])),
            (
                2,
                field("quantity", prim("u32"), &["#[serde(rename = \"qty\")]"]),
            ),
            (3, field("internal", prim("bool"), &["#[serde(skip)]"])),
        ],
        vec![],
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    let item = &e.components["Item"];
    assert!(item["properties"].get("qty").is_some());
    assert!(item["properties"].get("internal").is_none(), "skip omits");
    assert_eq!(item["required"], json!(["qty"]));
}

#[test]
fn option_and_default_make_a_field_optional() {
    let d = doc(
        vec![
            (1, struct_item("Item", &[2, 3, 4], &[])),
            (2, field("id", prim("u32"), &[])),
            (3, field("note", path("Option", 9, &[prim("bool")]), &[])),
            (4, field("tag", prim("u32"), &["#[serde(default)]"])),
        ],
        vec![],
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    assert_eq!(e.components["Item"]["required"], json!(["id"]));
    // Option unwraps to the inner schema rather than becoming a wrapper.
    assert_eq!(
        e.components["Item"]["properties"]["note"]["type"],
        "boolean"
    );
}

#[test]
fn containers_become_arrays_and_maps() {
    let d = doc(
        vec![
            (1, struct_item("Item", &[2, 3], &[])),
            (2, field("tags", path("Vec", 9, &[prim("bool")]), &[])),
            (
                3,
                field(
                    "meta",
                    path("HashMap", 10, &[prim("str"), prim("u32")]),
                    &[],
                ),
            ),
        ],
        vec![],
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    let props = &e.components["Item"]["properties"];
    assert_eq!(props["tags"]["type"], "array");
    assert_eq!(props["tags"]["items"]["type"], "boolean");
    assert_eq!(props["meta"]["type"], "object");
    assert_eq!(props["meta"]["additionalProperties"]["format"], "int32");
}

#[test]
fn generic_instantiation_is_monomorphised_into_its_own_component() {
    let d = doc(
        vec![
            (1, struct_item("Envelope", &[2], &[])),
            (
                2,
                field("body", path("ApiResponse", 3, &[path("Item", 5, &[])]), &[]),
            ),
            (3, generic_struct("ApiResponse", &["T"], &[4], &[])),
            (4, field("data", json!({ "generic": "T" }), &[])),
            (5, struct_item("Item", &[6], &[])),
            (6, field("id", prim("u32"), &[])),
        ],
        vec![],
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Envelope").expect("extracts");
    let mono = e
        .components
        .get("ApiResponse_Item")
        .expect("monomorphised component named for its argument");
    assert_eq!(
        mono["properties"]["data"]["$ref"],
        "#/components/schemas/Item"
    );
    assert!(e.gaps.is_empty());
}

#[test]
fn self_referential_types_terminate() {
    let d = doc(
        vec![
            (1, struct_item("Node", &[2, 3], &[])),
            (2, field("value", prim("u32"), &[])),
            (
                3,
                field("next", path("Option", 9, &[path("Node", 1, &[])]), &[]),
            ),
        ],
        vec![],
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Node").expect("terminates");
    assert_eq!(
        e.components["Node"]["properties"]["next"]["$ref"],
        "#/components/schemas/Node"
    );
}

#[test]
fn custom_serializer_reports_a_gap_and_emits_open() {
    let d = doc(
        vec![
            (1, struct_item("Item", &[2], &[])),
            (
                2,
                field("at", prim("u64"), &["#[serde(with = \"ts_seconds\")]"]),
            ),
        ],
        vec![],
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    assert_eq!(
        e.gaps,
        vec![Gap::CustomSerializer {
            at: "Item.at".into(),
            with: "ts_seconds".into()
        }]
    );
    // Open, not the u64 the type claims — the serializer decides the wire.
    assert_eq!(e.components["Item"]["properties"]["at"], json!({}));
}

#[test]
fn unmapped_foreign_type_is_named_exactly() {
    let d = doc(
        vec![
            (1, struct_item("Item", &[2], &[])),
            (2, field("cost", path("Money", 7, &[]), &[])),
        ],
        vec![(7, "exotic::money::Money")],
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    assert_eq!(
        e.gaps,
        vec![Gap::UnmappedForeign {
            at: "Item.cost".into(),
            path: "exotic::money::Money".into()
        }]
    );
}

#[test]
fn mapped_foreign_type_resolves_without_a_gap() {
    let d = doc(
        vec![
            (1, struct_item("Item", &[2], &[])),
            (2, field("id", path("Uuid", 7, &[]), &[])),
        ],
        vec![(7, "uuid::Uuid")],
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    assert!(e.gaps.is_empty(), "gaps: {:?}", e.gaps);
    assert_eq!(e.components["Item"]["properties"]["id"]["format"], "uuid");
}

#[test]
fn stripped_private_fields_are_reported_and_left_open() {
    let mut item = struct_item("Item", &[2], &[]);
    item["inner"]["struct"]["kind"]["plain"]["has_stripped_fields"] = json!(true);
    let d = doc(vec![(1, item), (2, field("id", prim("u32"), &[]))], vec![]);
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    assert_eq!(e.gaps, vec![Gap::StrippedFields { at: "Item".into() }]);
    assert_eq!(e.components["Item"]["additionalProperties"], json!(true));
}

// --- enum representations -------------------------------------------------

fn enum_doc(container_attrs: &[&str]) -> Value {
    doc(
        vec![
            (
                1,
                json!({
                    "name": "Event", "docs": Value::Null, "attrs": attrs_of(container_attrs),
                    "inner": { "enum": { "variants": [2, 3, 4],
                                         "generics": { "params": [], "where_predicates": [] } } },
                }),
            ),
            (
                2,
                json!({ "name": "Ping", "docs": Value::Null, "attrs": [],
                        "inner": { "variant": { "kind": "plain" } } }),
            ),
            (
                3,
                json!({ "name": "Text", "docs": Value::Null, "attrs": [],
                        "inner": { "variant": { "kind": { "tuple": [5] } } } }),
            ),
            (
                4,
                json!({ "name": "Detail", "docs": Value::Null, "attrs": [],
                        "inner": { "variant": { "kind": { "struct": {
                            "fields": [6], "has_stripped_fields": false } } } } }),
            ),
            (5, field("0", prim("str"), &[])),
            (6, field("code", prim("u32"), &[])),
        ],
        vec![],
    )
}

#[test]
fn externally_tagged_enum_is_serdes_default_shape() {
    let e = extract_named(&enum_doc(&[]), &TypeTable::builtin(), "Event").expect("extracts");
    let branches = e.components["Event"]["oneOf"].as_array().expect("oneOf");
    assert_eq!(
        branches[0],
        json!({ "const": "Ping" }),
        "unit variant is a bare string"
    );
    assert_eq!(branches[1]["properties"]["Text"]["type"], "string");
    assert_eq!(branches[2]["properties"]["Detail"]["type"], "object");
}

#[test]
fn internally_tagged_enum_merges_the_tag_into_the_object() {
    let d = enum_doc(&["#[serde(tag = \"kind\", rename_all = \"snake_case\")]"]);
    let e = extract_named(&d, &TypeTable::builtin(), "Event").expect("extracts");
    let branches = e.components["Event"]["oneOf"].as_array().expect("oneOf");
    assert_eq!(branches[0]["properties"]["kind"]["const"], "ping");
    let detail = &branches[2];
    assert_eq!(detail["properties"]["kind"]["const"], "detail");
    assert_eq!(detail["properties"]["code"]["format"], "int32");
    assert_eq!(detail["required"][0], "kind");
}

#[test]
fn adjacently_tagged_enum_separates_tag_and_content() {
    let d = enum_doc(&["#[serde(tag = \"t\", content = \"c\")]"]);
    let e = extract_named(&d, &TypeTable::builtin(), "Event").expect("extracts");
    let branches = e.components["Event"]["oneOf"].as_array().expect("oneOf");
    assert_eq!(branches[0]["properties"]["t"]["const"], "Ping");
    assert!(
        branches[0]["properties"].get("c").is_none(),
        "unit has no content"
    );
    assert_eq!(branches[1]["properties"]["c"]["type"], "string");
}

#[test]
fn internally_tagged_newtype_of_struct_keeps_its_payload() {
    // The payload resolves to a $ref, which has no properties to merge the
    // tag into; dropping it would silently lose every field of the variant.
    let d = doc(
        vec![
            (
                1,
                json!({
                    "name": "Event", "docs": Value::Null,
                    "attrs": attrs_of(&["#[serde(tag = \"kind\")]"]),
                    "inner": { "enum": { "variants": [2],
                                         "generics": { "params": [], "where_predicates": [] } } },
                }),
            ),
            (
                2,
                json!({ "name": "Created", "docs": Value::Null, "attrs": [],
                        "inner": { "variant": { "kind": { "tuple": [3] } } } }),
            ),
            (3, field("0", path("Item", 4, &[]), &[])),
            (4, struct_item("Item", &[5], &[])),
            (5, field("id", prim("u32"), &[])),
        ],
        vec![],
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Event").expect("extracts");
    let branch = &e.components["Event"]["oneOf"][0];
    let all = branch["allOf"].as_array().expect("composed, not dropped");
    assert_eq!(all[0]["properties"]["kind"]["const"], "Created");
    assert_eq!(all[1]["$ref"], "#/components/schemas/Item");
}

#[test]
fn untagged_enum_is_the_payload_alone() {
    let d = enum_doc(&["#[serde(untagged)]"]);
    let e = extract_named(&d, &TypeTable::builtin(), "Event").expect("extracts");
    let branches = e.components["Event"]["oneOf"].as_array().expect("oneOf");
    assert_eq!(
        branches[0],
        json!({ "type": "null" }),
        "unit variant is null"
    );
    assert_eq!(branches[1]["type"], "string");
}

#[test]
fn erased_return_types_report_a_gap_rather_than_guessing() {
    let d = doc(
        vec![
            (1, struct_item("Item", &[2], &[])),
            (
                2,
                field(
                    "body",
                    json!({ "impl_trait": [
                { "trait_bound": { "trait": { "path": "IntoResponse" } } }] }),
                    &[],
                ),
            ),
        ],
        vec![],
    );
    let e = extract_named(&d, &TypeTable::builtin(), "Item").expect("extracts");
    assert_eq!(
        e.gaps,
        vec![Gap::Erased {
            at: "Item.body".into(),
            bound: "IntoResponse".into()
        }]
    );
}

#[test]
fn unknown_type_name_is_an_error_not_an_empty_schema() {
    let d = doc(vec![(1, struct_item("Item", &[], &[]))], vec![]);
    assert!(extract_named(&d, &TypeTable::builtin(), "Nope").is_err());
}
