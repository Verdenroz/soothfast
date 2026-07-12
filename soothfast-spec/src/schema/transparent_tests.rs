//! `[spec.types]` entries mapped `transparent`.
//!
//! A transparent newtype wrapper (`#[serde(transparent)] struct Json<T>(T)`)
//! has no wire shape of its own, so a literal mapping for it would erase
//! whatever it wraps. Fixtures are hand-built rustdoc JSON, as in
//! [`super::extract_tests`].

use serde_json::{Value, json};

use super::{Docs, Gap, TypeMapping, TypeTable, extract_named};

/// A rustdoc document: items this crate defines, plus paths it only names.
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

fn struct_item(name: &str, fields: &[u64]) -> Value {
    json!({
        "name": name, "docs": Value::Null, "attrs": [],
        "inner": { "struct": {
            "kind": { "plain": { "fields": fields, "has_stripped_fields": false } },
            "generics": { "params": [], "where_predicates": [] },
        }},
    })
}

fn field(name: &str, ty: Value) -> Value {
    json!({ "name": name, "docs": Value::Null, "attrs": [],
            "inner": { "struct_field": ty } })
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

const JSON_PATH: &str = "async_graphql::types::json::Json";

/// A table mapping the async-graphql wrapper transparent.
fn transparent_table() -> TypeTable {
    let mut t = TypeTable::builtin();
    t.insert(JSON_PATH, TypeMapping::Transparent { arg: 0 });
    t
}

/// `struct Quote { price: Json<ARG> }`, with the wrapper foreign.
fn quote_wrapping(arg: Value) -> Value {
    doc(
        vec![
            (1, struct_item("Quote", &[2])),
            (2, field("price", path("Json", 9, &[arg]))),
        ],
        vec![(9, JSON_PATH)],
    )
}

#[test]
fn forwards_to_a_walkable_local_type_instead_of_erasing_it() {
    let mut index = vec![
        (1, struct_item("Quote", &[2])),
        (
            2,
            field("price", path("Json", 9, &[path("Formatted", 3, &[])])),
        ),
        (3, struct_item("Formatted", &[4])),
        (4, field("raw", json!({ "primitive": "f64" }))),
    ];
    index.sort_by_key(|(id, _)| *id);
    let d = doc(index, vec![(9, JSON_PATH)]);

    let e = extract_named(&d, &transparent_table(), "Quote").expect("extracts");
    assert!(e.gaps.is_empty(), "forwarded, so not a gap: {:?}", e.gaps);
    assert_eq!(
        e.components["Quote"]["properties"]["price"],
        json!({ "$ref": "#/components/schemas/Formatted" }),
        "the wrapper contributes no component of its own"
    );
    assert_eq!(
        e.components["Formatted"]["properties"]["raw"]["type"],
        "number"
    );
    assert!(
        !e.components.contains_key("Json"),
        "no wrapper component: {:?}",
        e.components.keys().collect::<Vec<_>>()
    );
}

#[test]
fn forwards_to_a_builtin_mapped_type_so_arbitrary_json_stays_open() {
    // The ~29 genuinely-unconstrained sites: `serde_json::Value` maps to `{}`
    // in the builtin table, which is the honest answer, not a gap.
    let d = quote_wrapping(path("Value", 10, &[]));
    let mut table = transparent_table();
    table.insert("serde_json::value::Value", json!({}));
    let mut d = d;
    d["paths"]["10"] = json!({ "crate_id": 1, "path": ["serde_json", "value", "Value"],
                               "kind": "enum" });

    let e = extract_named(&d, &table, "Quote").expect("extracts");
    assert!(e.gaps.is_empty(), "an open schema by mapping: {:?}", e.gaps);
    assert_eq!(e.components["Quote"]["properties"]["price"], json!({}));
}

#[test]
fn forwards_through_nested_generics() {
    // `Json<Vec<Formatted>>`: the argument is resolved structurally, so the
    // container survives the forward.
    let d = doc(
        vec![
            (1, struct_item("Quote", &[2])),
            (
                2,
                field(
                    "history",
                    path("Json", 9, &[path("Vec", 11, &[path("Formatted", 3, &[])])]),
                ),
            ),
            (3, struct_item("Formatted", &[4])),
            (4, field("raw", json!({ "primitive": "f64" }))),
        ],
        vec![(9, JSON_PATH)],
    );

    let e = extract_named(&d, &transparent_table(), "Quote").expect("extracts");
    assert!(e.gaps.is_empty(), "{:?}", e.gaps);
    assert_eq!(
        e.components["Quote"]["properties"]["history"],
        json!({ "type": "array", "items": { "$ref": "#/components/schemas/Formatted" } })
    );
}

#[test]
fn forwards_a_generic_parameter_through_the_use_sites_substitution() {
    // `Envelope<Formatted> { body: Json<T> }` — the wrapper's argument is the
    // outer type's parameter, so it must resolve in the caller's context.
    let d = doc(
        vec![
            (
                1,
                json!({
                    "name": "Envelope", "docs": Value::Null, "attrs": [],
                    "inner": { "struct": {
                        "kind": { "plain": { "fields": [2], "has_stripped_fields": false } },
                        "generics": { "params": [{ "name": "T", "kind": { "type": {} } }],
                                      "where_predicates": [] },
                    }},
                }),
            ),
            (
                2,
                field("body", path("Json", 9, &[json!({ "generic": "T" })])),
            ),
            (3, struct_item("Formatted", &[4])),
            (4, field("raw", json!({ "primitive": "f64" }))),
            (5, struct_item("Req", &[6])),
            (
                6,
                field("env", path("Envelope", 1, &[path("Formatted", 3, &[])])),
            ),
        ],
        vec![(9, JSON_PATH)],
    );

    let e = extract_named(&d, &transparent_table(), "Req").expect("extracts");
    assert!(e.gaps.is_empty(), "{:?}", e.gaps);
    assert_eq!(
        e.components["Envelope_Formatted"]["properties"]["body"],
        json!({ "$ref": "#/components/schemas/Formatted" })
    );
}

#[test]
fn a_transparent_type_used_with_no_argument_is_a_gap_not_a_guess() {
    let d = quote_wrapping(json!(null));
    // Strip the argument entirely: `Json` with no `<..>` at all.
    let mut d = d;
    d["index"]["2"]["inner"]["struct_field"] = path("Json", 9, &[]);

    let e = extract_named(&d, &transparent_table(), "Quote").expect("extracts");
    assert_eq!(e.components["Quote"]["properties"]["price"], json!({}));
    assert!(
        matches!(
            e.gaps.as_slice(),
            [Gap::TransparentWithoutArgument { path, arg, .. }]
                if path == JSON_PATH && *arg == 0
        ),
        "got {:?}",
        e.gaps
    );
    assert!(
        e.gaps[0].explain().contains("transparent"),
        "the report names the cause: {}",
        e.gaps[0].explain()
    );
}

#[test]
fn an_explicit_index_forwards_the_named_argument() {
    let mut table = TypeTable::builtin();
    table.insert("w::Tagged", TypeMapping::Transparent { arg: 1 });
    let d = doc(
        vec![
            (1, struct_item("Req", &[2])),
            (
                2,
                field(
                    "body",
                    path(
                        "Tagged",
                        9,
                        &[json!({ "primitive": "u32" }), path("Formatted", 3, &[])],
                    ),
                ),
            ),
            (3, struct_item("Formatted", &[4])),
            (4, field("raw", json!({ "primitive": "f64" }))),
        ],
        vec![(9, "w::Tagged")],
    );

    let e = extract_named(&d, &table, "Req").expect("extracts");
    assert_eq!(
        e.components["Req"]["properties"]["body"],
        json!({ "$ref": "#/components/schemas/Formatted" }),
        "the second argument, not the first"
    );
}

#[test]
fn a_literal_mapping_still_wins_where_one_is_declared() {
    // Proof the directive did not change what a literal entry does.
    let d = quote_wrapping(path("Formatted", 3, &[]));
    let mut table = TypeTable::builtin();
    table.insert(JSON_PATH, json!({}));

    let e = extract_named(&d, &table, "Quote").expect("extracts");
    assert!(e.gaps.is_empty(), "mapped, so not a gap: {:?}", e.gaps);
    assert_eq!(e.components["Quote"]["properties"]["price"], json!({}));
}

#[test]
fn the_same_wrapper_forwards_to_a_sibling_crates_type() {
    // The wrapper is foreign and its argument lives in a workspace crate:
    // forwarding must hand off to the aux document like any other type.
    let server = doc(
        vec![
            (1, struct_item("Quote", &[2])),
            (
                2,
                field("price", path("Json", 9, &[path("Formatted", 8, &[])])),
            ),
        ],
        vec![(9, JSON_PATH), (8, "lib_crate::model::Formatted")],
    );
    let mut lib = doc(
        vec![
            (5, struct_item("Formatted", &[6])),
            (6, field("raw", json!({ "primitive": "f64" }))),
        ],
        vec![],
    );
    lib["root"] = json!("0");
    lib["paths"]["0"] = json!({ "crate_id": 0, "path": ["lib_crate"], "kind": "module" });
    lib["paths"]["5"] = json!({ "crate_id": 0, "path": ["lib_crate", "model", "Formatted"],
                                "kind": "struct" });
    let aux = [lib];

    let e =
        extract_named(Docs::new(&server, &aux), &transparent_table(), "Quote").expect("extracts");
    assert!(e.gaps.is_empty(), "{:?}", e.gaps);
    assert_eq!(
        e.components["Quote"]["properties"]["price"],
        json!({ "$ref": "#/components/schemas/Formatted" })
    );
    assert!(e.aux_resolved.contains("lib_crate::model::Formatted"));
}
