//! Resolution across several crates of one cargo workspace.
//!
//! Fixtures are hand-built the same way [`super::extract_tests`] builds
//! them — two documents here, standing in for a package and a sibling crate
//! it takes its types from.

use serde_json::{Value, json};

use super::{Docs, Gap, TypeTable, extract_named};

/// A rustdoc document for one crate: `krate` is its own name, `local` the
/// items it defines (id, path-under-the-crate, item), `foreign` the paths it
/// only *names* (id, canonical path).
fn krate(name: &str, local: Vec<(u64, &str, Value)>, foreign: Vec<(u64, &str)>) -> Value {
    let mut index = serde_json::Map::new();
    let mut paths = serde_json::Map::new();
    paths.insert(
        "0".into(),
        json!({ "crate_id": 0, "path": [name], "kind": "module" }),
    );
    for (id, path, item) in local {
        let kind = if item["inner"].get("enum").is_some() {
            "enum"
        } else {
            "struct"
        };
        if !path.is_empty() {
            let segs: Vec<&str> = path.split("::").collect();
            paths.insert(
                id.to_string(),
                json!({ "crate_id": 0, "path": segs, "kind": kind }),
            );
        }
        index.insert(id.to_string(), item);
    }
    for (id, path) in foreign {
        let segs: Vec<&str> = path.split("::").collect();
        paths.insert(
            id.to_string(),
            json!({ "crate_id": 1, "path": segs, "kind": "enum" }),
        );
    }
    json!({ "root": "0", "index": index, "paths": paths })
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

fn unit_enum(name: &str, variants: &[(u64, &str)]) -> Vec<(u64, Value)> {
    let ids: Vec<u64> = variants.iter().map(|(id, _)| *id).collect();
    let mut out = vec![(
        0,
        json!({
            "name": name, "docs": Value::Null, "attrs": [],
            "inner": { "enum": { "variants": ids,
                                 "generics": { "params": [], "where_predicates": [] } } },
        }),
    )];
    for (id, vname) in variants {
        out.push((
            *id,
            json!({ "name": vname, "docs": Value::Null, "attrs": [],
                    "inner": { "variant": { "kind": "plain" } } }),
        ));
    }
    out
}

fn path_ty(name: &str, id: u64) -> Value {
    json!({ "resolved_path": { "path": name, "id": id, "args": null } })
}

/// The motivating layout: a server crate whose query struct is typed by an
/// enum defined in the workspace's root library crate.
fn server_and_lib() -> (Value, Value) {
    let server = krate(
        "server",
        vec![
            (
                1,
                "server::params::ChartQuery",
                struct_item("ChartQuery", &[2]),
            ),
            (2, "", field("interval", path_ty("Interval", 9))),
        ],
        vec![(9, "finance_query::constants::Interval")],
    );
    let mut lib_items = vec![];
    for (id, item) in unit_enum("Interval", &[(6, "OneDay"), (7, "OneWeek")]) {
        let path = if id == 0 {
            "finance_query::constants::Interval"
        } else {
            ""
        };
        lib_items.push((if id == 0 { 5 } else { id }, path, item));
    }
    let lib = krate("finance_query", lib_items, vec![]);
    (server, lib)
}

#[test]
fn a_sibling_crates_enum_resolves_instead_of_becoming_a_gap() {
    let (server, lib) = server_and_lib();
    let aux = [lib];
    let e = extract_named(
        Docs::new(&server, &aux),
        &TypeTable::builtin(),
        "ChartQuery",
    )
    .expect("extracts");

    assert!(e.gaps.is_empty(), "resolved, so not a gap: {:?}", e.gaps);
    assert_eq!(
        e.components["ChartQuery"]["properties"]["interval"],
        json!({ "$ref": "#/components/schemas/Interval" })
    );
    assert_eq!(
        e.components["Interval"],
        json!({ "type": "string", "enum": ["OneDay", "OneWeek"] }),
        "the enum constraint is derived, not flattened to a bare string"
    );
    assert!(
        e.aux_resolved
            .contains("finance_query::constants::Interval")
    );
}

#[test]
fn without_the_sibling_document_the_type_is_still_a_gap() {
    let (server, _) = server_and_lib();
    let e = extract_named(&server, &TypeTable::builtin(), "ChartQuery").expect("extracts");

    assert_eq!(
        e.components["ChartQuery"]["properties"]["interval"],
        json!({})
    );
    assert!(matches!(
        e.gaps.as_slice(),
        [Gap::UnmappedForeign { path, .. }] if path == "finance_query::constants::Interval"
    ));
    assert!(e.aux_resolved.is_empty());
}

#[test]
fn a_type_table_entry_still_outranks_the_sibling_crate() {
    let (server, lib) = server_and_lib();
    let aux = [lib];
    let mut table = TypeTable::builtin();
    table.insert(
        "finance_query::constants::Interval",
        json!({ "type": "string", "format": "duration" }),
    );
    let e = extract_named(Docs::new(&server, &aux), &table, "ChartQuery").expect("extracts");

    assert_eq!(
        e.components["ChartQuery"]["properties"]["interval"],
        json!({ "type": "string", "format": "duration" }),
        "the documented escape hatch has to keep winning"
    );
    assert!(!e.components.contains_key("Interval"));
    assert!(e.aux_resolved.is_empty());
}

#[test]
fn a_chain_of_types_keeps_walking_inside_the_sibling_crate() {
    // Reaching `Outer` in the sibling crate must resolve `Inner` there too:
    // the ids in `Outer`'s fields belong to that document, not this one.
    let server = krate(
        "server",
        vec![
            (1, "server::Req", struct_item("Req", &[2])),
            (2, "", field("payload", path_ty("Outer", 9))),
        ],
        vec![(9, "lib_crate::model::Outer")],
    );
    let lib = krate(
        "lib_crate",
        vec![
            (5, "lib_crate::model::Outer", struct_item("Outer", &[6])),
            (6, "", field("inner", path_ty("Inner", 7))),
            (7, "lib_crate::model::Inner", struct_item("Inner", &[8])),
            (8, "", field("code", json!({ "primitive": "u32" }))),
        ],
        vec![],
    );
    let aux = [lib];
    let e =
        extract_named(Docs::new(&server, &aux), &TypeTable::builtin(), "Req").expect("extracts");

    assert!(e.gaps.is_empty(), "{:?}", e.gaps);
    assert_eq!(
        e.components["Outer"]["properties"]["inner"],
        json!({ "$ref": "#/components/schemas/Inner" })
    );
    assert_eq!(
        e.components["Inner"]["properties"]["code"]["format"],
        "int32"
    );
    // Only the crossing is a jump: `Inner` is local to the document `Outer`
    // was walked out of, so it never went through foreign resolution.
    assert_eq!(
        e.aux_resolved.iter().collect::<Vec<_>>(),
        vec!["lib_crate::model::Outer"]
    );
}

#[test]
fn two_crates_same_named_types_get_two_components() {
    // The regression this guards: one `Meta` silently standing in for the
    // other, so half the spec describes the wrong shape.
    let server = krate(
        "server",
        vec![
            (1, "server::Req", struct_item("Req", &[2, 3])),
            (2, "", field("own", path_ty("Meta", 4))),
            (3, "", field("theirs", path_ty("Meta", 9))),
            (4, "server::Meta", struct_item("Meta", &[5])),
            (5, "", field("local_only", json!({ "primitive": "bool" }))),
        ],
        vec![(9, "lib_crate::Meta")],
    );
    let lib = krate(
        "lib_crate",
        vec![
            (6, "lib_crate::Meta", struct_item("Meta", &[7])),
            (7, "", field("foreign_only", json!({ "primitive": "u32" }))),
        ],
        vec![],
    );
    let aux = [lib];
    let e =
        extract_named(Docs::new(&server, &aux), &TypeTable::builtin(), "Req").expect("extracts");

    // Which one keeps the bare name is decided by canonical path order, not
    // by which field reached it first, so it cannot move between runs.
    assert_eq!(
        e.components["Req"]["properties"]["own"],
        json!({ "$ref": "#/components/schemas/server_Meta" })
    );
    assert_eq!(
        e.components["Req"]["properties"]["theirs"],
        json!({ "$ref": "#/components/schemas/Meta" }),
        "distinct types, distinct components — never merged"
    );
    assert!(
        e.components["server_Meta"]["properties"]
            .get("local_only")
            .is_some()
    );
    assert!(
        e.components["Meta"]["properties"]
            .get("foreign_only")
            .is_some()
    );
}

#[test]
fn a_registry_dependency_is_untouched_by_workspace_resolution() {
    let server = krate(
        "server",
        vec![
            (1, "server::Req", struct_item("Req", &[2])),
            (2, "", field("when", path_ty("Exotic", 9))),
        ],
        vec![(9, "some_registry_crate::Exotic")],
    );
    let lib = krate("lib_crate", vec![], vec![]);
    let aux = [lib];
    let e = extract_named(Docs::new(&server, &aux), &TypeTable::empty(), "Req").expect("extracts");

    assert_eq!(e.components["Req"]["properties"]["when"], json!({}));
    assert!(matches!(
        e.gaps.as_slice(),
        [Gap::UnmappedForeign { path, .. }] if path == "some_registry_crate::Exotic"
    ));
}

#[test]
fn a_response_override_can_name_a_sibling_crates_type() {
    let server = krate("server", vec![], vec![]);
    let lib = krate(
        "lib_crate",
        vec![
            (
                5,
                "lib_crate::streaming::PriceUpdate",
                struct_item("PriceUpdate", &[6]),
            ),
            (6, "", field("price", json!({ "primitive": "f64" }))),
        ],
        vec![],
    );
    let aux = [lib];
    let e = extract_named(
        Docs::new(&server, &aux),
        &TypeTable::builtin(),
        "PriceUpdate",
    )
    .expect("a bare type name resolves across the workspace");
    assert_eq!(
        e.schema,
        json!({ "$ref": "#/components/schemas/PriceUpdate" })
    );
    assert_eq!(
        e.components["PriceUpdate"]["properties"]["price"]["format"],
        "double"
    );
}

#[test]
fn a_document_that_is_not_rustdoc_json_is_skipped_not_fatal() {
    let (server, _) = server_and_lib();
    let aux = [json!({ "oops": true })];
    let e = extract_named(
        Docs::new(&server, &aux),
        &TypeTable::builtin(),
        "ChartQuery",
    )
    .expect("a junk auxiliary document degrades to the old behaviour");
    assert_eq!(e.gaps.len(), 1);
}

#[test]
fn naming_is_stable_across_runs_and_repeat_visits() {
    // Golden specs are byte-compared, so a component name must not depend on
    // how many times a type was reached or on which run reached it.
    let server = krate(
        "server",
        vec![
            (1, "server::Req", struct_item("Req", &[2, 3])),
            (2, "", field("first", path_ty("Meta", 9))),
            (3, "", field("second", path_ty("Meta", 9))),
        ],
        vec![(9, "lib_crate::Meta")],
    );
    let lib = krate(
        "lib_crate",
        vec![
            (6, "lib_crate::Meta", struct_item("Meta", &[7])),
            (7, "", field("n", json!({ "primitive": "u32" }))),
        ],
        vec![],
    );
    let aux = [lib];
    let once = extract_named(Docs::new(&server, &aux), &TypeTable::builtin(), "Req")
        .expect("extracts")
        .components;
    let twice = extract_named(Docs::new(&server, &aux), &TypeTable::builtin(), "Req")
        .expect("extracts")
        .components;

    assert_eq!(once, twice);
    assert_eq!(
        once.keys().collect::<Vec<_>>(),
        vec!["Meta", "Req"],
        "one type, one component, however many fields point at it"
    );
}

#[test]
fn two_operations_name_two_same_named_types_compatibly() {
    // The regression this guards: finance-query defines `Region` twice in one
    // crate (`constants::Region` and `constants::indices::Region`). Each is
    // reached by a different route, so each is resolved by its own Resolver —
    // and the dialects then merge every operation's components into one
    // document, which hard-fails if one name carries two bodies.
    let lib = krate(
        "fq",
        vec![
            (1, "fq::constants::Region", struct_item("Region", &[2])),
            (2, "", field("code", json!({ "primitive": "u32" }))),
            (
                3,
                "fq::constants::indices::Region",
                struct_item("Region", &[4]),
            ),
            (4, "", field("slug", json!({ "primitive": "str" }))),
        ],
        vec![],
    );
    let get_hours = krate(
        "server",
        vec![
            (10, "server::HoursReply", struct_item("HoursReply", &[11])),
            (11, "", field("region", path_ty("Region", 90))),
        ],
        vec![(90, "fq::constants::Region")],
    );
    let get_indices = krate(
        "server",
        vec![
            (
                10,
                "server::IndicesReply",
                struct_item("IndicesReply", &[11]),
            ),
            (11, "", field("region", path_ty("Region", 90))),
        ],
        vec![(90, "fq::constants::indices::Region")],
    );

    let aux = [lib];
    let hours = extract_named(
        Docs::new(&get_hours, &aux),
        &TypeTable::builtin(),
        "HoursReply",
    )
    .expect("extracts");
    let indices = extract_named(
        Docs::new(&get_indices, &aux),
        &TypeTable::builtin(),
        "IndicesReply",
    )
    .expect("extracts");

    assert_eq!(
        hours.components["Region"]["properties"]["code"]["format"],
        "int32"
    );
    assert_eq!(
        indices.components["indices_Region"]["properties"]["slug"]["type"],
        "string"
    );
    // Merging the two operations' components is exactly what a dialect does,
    // and no name may carry two different bodies afterwards.
    for (name, body) in &indices.components {
        if let Some(other) = hours.components.get(name) {
            assert_eq!(other, body, "component `{name}` has two definitions");
        }
    }
}

#[test]
fn a_types_name_does_not_depend_on_which_operation_reached_it_first() {
    // Same two types, resolved in the opposite order: the names must not move.
    let lib = krate(
        "fq",
        vec![
            (1, "fq::constants::Region", struct_item("Region", &[2])),
            (2, "", field("code", json!({ "primitive": "u32" }))),
            (
                3,
                "fq::constants::indices::Region",
                struct_item("Region", &[4]),
            ),
            (4, "", field("slug", json!({ "primitive": "str" }))),
        ],
        vec![],
    );
    let aux = [lib];
    let reply = |canonical: &str| {
        krate(
            "server",
            vec![
                (10, "server::Reply", struct_item("Reply", &[11])),
                (11, "", field("region", path_ty("Region", 90))),
            ],
            vec![(90, canonical)],
        )
    };

    let indices_doc = reply("fq::constants::indices::Region");
    let plain_doc = reply("fq::constants::Region");
    let names = |doc: &Value| {
        extract_named(Docs::new(doc, &aux), &TypeTable::builtin(), "Reply")
            .expect("extracts")
            .components
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };

    // Reaching `indices::Region` first must not let it take the bare name.
    assert_eq!(names(&indices_doc), vec!["Reply", "indices_Region"]);
    assert_eq!(names(&plain_doc), vec!["Region", "Reply"]);
}
