//! Route signature inference tests over synthetic rustdoc JSON.
//!
//! Shapes are pinned against a live nightly probe; synthetic input keeps the
//! suite off nightly and stable across rustdoc releases.

use serde_json::{Value, json};

use super::route_sig::{Extractors, Overrides, Role, infer};
use super::{Gap, TypeTable};

/// Handler `app::handler` plus whatever types the signature references.
fn doc(inputs: Vec<(&str, Value)>, output: Value, extra: Vec<(u64, Value)>) -> Value {
    let inputs: Vec<Value> = inputs.into_iter().map(|(n, t)| json!([n, t])).collect();
    let mut index = serde_json::Map::new();
    index.insert(
        "1".into(),
        json!({ "name": "handler", "docs": Value::Null, "attrs": [],
                "inner": { "function": { "sig": { "inputs": inputs, "output": output } } } }),
    );
    for (id, item) in extra {
        index.insert(id.to_string(), item);
    }
    json!({
        "index": index,
        "paths": { "1": { "crate_id": 0, "path": ["app", "handler"], "kind": "function" } },
    })
}

fn struct_item(name: &str, fields: &[u64], attrs: &[Value]) -> Value {
    json!({
        "name": name, "docs": Value::Null, "attrs": attrs,
        "inner": { "struct": {
            "kind": { "plain": { "fields": fields, "has_stripped_fields": false } },
            "generics": { "params": [], "where_predicates": [] } } },
    })
}

fn field(name: &str, ty: Value) -> Value {
    json!({ "name": name, "docs": Value::Null, "attrs": [],
            "inner": { "struct_field": ty } })
}

fn prim(p: &str) -> Value {
    json!({ "primitive": p })
}

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

/// `Item { id: u32, note: Option<bool> }` at id 20.
fn item_type() -> Vec<(u64, Value)> {
    vec![
        (20, struct_item("Item", &[21, 22], &[])),
        (21, field("id", prim("u32"))),
        (22, field("note", path("Option", 99, &[prim("bool")]))),
    ]
}

fn run(d: &Value, route: &str, o: &Overrides) -> super::RouteShape {
    infer(
        d,
        &TypeTable::builtin(),
        &Extractors::builtin(),
        "app::handler",
        route,
        o,
    )
    .expect("infers")
}

#[test]
fn body_extractor_becomes_the_request() {
    let d = doc(
        vec![("b", path("Json", 5, &[path("Item", 20, &[])]))],
        path("Json", 5, &[path("Item", 20, &[])]),
        item_type(),
    );
    let s = run(&d, "/items", &Overrides::default());
    let body = s.request.expect("has a body");
    assert_eq!(body.content_type, "application/json");
    assert_eq!(body.schema["$ref"], "#/components/schemas/Item");
}

#[test]
fn ambient_context_parameters_are_not_part_of_the_contract() {
    let d = doc(
        vec![
            ("st", path("State", 6, &[prim("u8")])),
            ("ext", path("Extension", 7, &[prim("u8")])),
        ],
        Value::Null,
        vec![],
    );
    let s = run(&d, "/items", &Overrides::default());
    assert!(s.request.is_none());
    assert!(s.parameters.is_empty(), "got {:?}", s.parameters);
}

#[test]
fn query_struct_expands_into_parameters_with_required_flags() {
    let d = doc(
        vec![("q", path("Query", 5, &[path("Item", 20, &[])]))],
        Value::Null,
        item_type(),
    );
    let s = run(&d, "/items", &Overrides::default());
    let names: Vec<&str> = s.parameters.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["id", "note"]);
    assert!(s.parameters.iter().all(|p| p.location == "query"));
    assert!(s.parameters[0].required, "id is required");
    assert!(!s.parameters[1].required, "Option field is optional");
}

#[test]
fn tuple_path_params_take_their_names_from_the_route_template() {
    let ty = json!({ "tuple": [prim("u32"), prim("str")] });
    let d = doc(vec![("p", path("Path", 5, &[ty]))], Value::Null, vec![]);
    let s = run(&d, "/items/{id}/tags/{tag}", &Overrides::default());
    let names: Vec<&str> = s.parameters.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["id", "tag"]);
    assert!(s.parameters.iter().all(|p| p.location == "path"));
}

#[test]
fn scalar_path_param_takes_the_sole_placeholder() {
    let d = doc(
        vec![("p", path("Path", 5, &[prim("u32")]))],
        Value::Null,
        vec![],
    );
    let s = run(&d, "/items/{id}", &Overrides::default());
    assert_eq!(s.parameters.len(), 1);
    assert_eq!(s.parameters[0].name, "id");
    assert_eq!(s.parameters[0].schema["format"], "int32");
}

#[test]
fn result_contributes_both_success_and_error_responses() {
    let ok = path("Json", 5, &[path("Item", 20, &[])]);
    let err = path("ApiError", 30, &[]);
    let mut extra = item_type();
    extra.push((30, struct_item("ApiError", &[31], &[])));
    extra.push((31, field("message", prim("str"))));
    let d = doc(vec![], path("Result", 1, &[ok, err]), extra);
    let s = run(&d, "/items", &Overrides::default());
    assert_eq!(
        s.responses["200"].schema["$ref"],
        "#/components/schemas/Item"
    );
    assert_eq!(
        s.responses["default"].schema["$ref"],
        "#/components/schemas/ApiError"
    );
}

#[test]
fn status_code_tuple_still_yields_the_body() {
    let out = json!({ "tuple": [
        path("StatusCode", 40, &[]),
        path("Json", 5, &[path("Item", 20, &[])]),
    ]});
    let d = doc(vec![], out, item_type());
    let s = run(&d, "/items", &Overrides::default());
    assert_eq!(
        s.responses["200"].schema["$ref"],
        "#/components/schemas/Item"
    );
}

#[test]
fn unit_return_is_a_no_content_response() {
    let d = doc(vec![], Value::Null, vec![]);
    let s = run(&d, "/items", &Overrides::default());
    assert!(s.responses.contains_key("204"));
    assert_eq!(s.responses["204"].content_type, "");
}

#[test]
fn erased_return_reports_a_gap_and_leaves_the_response_open() {
    let out = json!({ "impl_trait": [
        { "trait_bound": { "trait": { "path": "IntoResponse" } } }] });
    let d = doc(vec![], out, vec![]);
    let s = run(&d, "/items", &Overrides::default());
    assert_eq!(s.responses["200"].schema, json!({}));
    assert!(
        matches!(s.gaps.first(), Some(Gap::Erased { .. })),
        "got {:?}",
        s.gaps
    );
}

#[test]
fn response_override_replaces_the_erased_shape_and_retires_its_gap() {
    let out = json!({ "impl_trait": [
        { "trait_bound": { "trait": { "path": "IntoResponse" } } }] });
    let d = doc(vec![], out, item_type());
    let o = Overrides {
        response: Some("Item".into()),
        status: Some(201),
        ..Default::default()
    };
    let s = run(&d, "/items", &o);
    assert_eq!(
        s.responses["201"].schema["$ref"],
        "#/components/schemas/Item"
    );
    assert!(!s.responses.contains_key("200"), "the open 200 is replaced");
    assert!(s.gaps.is_empty(), "override answers the gap: {:?}", s.gaps);
}

#[test]
fn status_override_alone_relabels_the_success_response() {
    let d = doc(
        vec![],
        path("Json", 5, &[path("Item", 20, &[])]),
        item_type(),
    );
    let o = Overrides {
        status: Some(201),
        ..Default::default()
    };
    let s = run(&d, "/items", &o);
    assert_eq!(
        s.responses["201"].schema["$ref"],
        "#/components/schemas/Item"
    );
    assert!(!s.responses.contains_key("200"));
}

#[test]
fn a_missing_handler_names_the_private_items_remedy() {
    let d = doc(vec![], Value::Null, vec![]);
    let err = infer(
        &d,
        &TypeTable::builtin(),
        &Extractors::builtin(),
        "app::nope",
        "/x",
        &Overrides::default(),
    )
    .expect_err("absent handler is an error");
    assert!(err.contains("--document-private-items"), "got {err}");
}

#[test]
fn unlisted_frameworks_cost_a_config_line_not_a_code_change() {
    let mut ex = Extractors::builtin();
    ex.insert("Payload", Role::Body("application/cbor".into()));
    let d = doc(
        vec![("b", path("Payload", 5, &[path("Item", 20, &[])]))],
        Value::Null,
        item_type(),
    );
    let s = infer(
        &d,
        &TypeTable::builtin(),
        &ex,
        "app::handler",
        "/x",
        &Overrides::default(),
    )
    .expect("infers");
    let body = s.request.expect("custom extractor recognised");
    assert_eq!(body.content_type, "application/cbor");
}

/// A doc containing only types — what a marker fn in a bench target sees:
/// its own item is absent because rustdoc documented the lib alone.
fn doc_types_only(extra: Vec<(u64, Value)>) -> Value {
    let mut index = serde_json::Map::new();
    for (id, item) in extra {
        index.insert(id.to_string(), item);
    }
    json!({ "index": index, "paths": {} })
}

#[test]
fn params_override_expands_a_struct_into_query_parameters() {
    let d = doc(
        vec![],
        path("Json", 5, &[path("Item", 20, &[])]),
        item_type(),
    );
    let o = Overrides {
        params: Some("Item".into()),
        ..Default::default()
    };
    let s = run(&d, "/items", &o);
    let names: Vec<&str> = s.parameters.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["id", "note"], "got {:?}", s.parameters);
    assert!(s.parameters.iter().all(|p| p.location == "query"));
    assert!(s.parameters[0].required, "non-Option field is required");
    assert!(!s.parameters[1].required, "Option field is not required");
}

#[test]
fn a_detached_marker_builds_its_whole_shape_from_overrides() {
    let d = doc_types_only(item_type());
    let o = Overrides {
        response: Some("Item".into()),
        params: Some("Item".into()),
        ..Default::default()
    };
    let s = infer(
        &d,
        &TypeTable::builtin(),
        &Extractors::builtin(),
        "bench::route_get_item",
        "/items/{key}",
        &o,
    )
    .expect("detached shape builds");
    assert_eq!(
        s.responses["200"].schema["$ref"],
        "#/components/schemas/Item"
    );
    let (paths, queries): (Vec<_>, Vec<_>) =
        s.parameters.iter().partition(|p| p.location == "path");
    assert_eq!(
        queries.len(),
        2,
        "params struct expanded: {:?}",
        s.parameters
    );
    assert_eq!(
        paths.len(),
        1,
        "placeholder synthesized: {:?}",
        s.parameters
    );
    assert_eq!(paths[0].name, "key");
    assert_eq!(paths[0].schema["type"], "string");
    assert!(paths[0].required);
}

#[test]
fn a_detached_marker_without_a_response_override_still_errors() {
    let d = doc_types_only(item_type());
    let err = infer(
        &d,
        &TypeTable::builtin(),
        &Extractors::builtin(),
        "bench::route_get_item",
        "/items",
        &Overrides::default(),
    )
    .expect_err("no overrides means a typo, not a marker");
    assert!(err.contains("response = "), "names the remedy: {err}");
}

#[test]
fn a_detached_marker_with_an_unknown_type_errors_instead_of_emitting_nothing() {
    let d = doc_types_only(item_type());
    let o = Overrides {
        response: Some("Nope".into()),
        ..Default::default()
    };
    let err = infer(
        &d,
        &TypeTable::builtin(),
        &Extractors::builtin(),
        "bench::route_get_item",
        "/items",
        &o,
    )
    .expect_err("a typo'd type must not produce an empty operation");
    assert!(err.contains("`Nope`"), "names the type: {err}");
}

#[test]
fn template_placeholders_synthesize_path_parameters_for_bare_signatures() {
    let d = doc(vec![], Value::Null, vec![]);
    let s = run(&d, "/items/{id}/tags/{tag}", &Overrides::default());
    let names: Vec<&str> = s.parameters.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["id", "tag"], "got {:?}", s.parameters);
    assert!(s.parameters.iter().all(|p| p.location == "path"));
}

/// A C-like enum at `id`, its variants at `id + 1 ..`.
fn unit_enum(name: &str, id: u64, variants: &[&str]) -> Vec<(u64, Value)> {
    let ids: Vec<u64> = (1..=variants.len() as u64).map(|i| id + i).collect();
    let mut out = vec![(
        id,
        json!({
            "name": name, "docs": Value::Null, "attrs": [],
            "inner": { "enum": { "variants": ids,
                                 "generics": { "params": [], "where_predicates": [] } } },
        }),
    )];
    for (i, v) in variants.iter().enumerate() {
        out.push((
            id + 1 + i as u64,
            json!({ "name": v, "docs": Value::Null, "attrs": [],
                    "inner": { "variant": { "kind": "plain" } } }),
        ));
    }
    out
}

/// `Sector` (a closed enum at 60) plus a params struct naming it.
fn sector_params(struct_name: &str, field_name: &str) -> Vec<(u64, Value)> {
    let mut extra = vec![
        (50, struct_item(struct_name, &[51, 52], &[])),
        (51, field(field_name, path("Sector", 60, &[]))),
        (52, field("fields", prim("str"))),
    ];
    extra.extend(unit_enum("Sector", 60, &["Tech", "Energy"]));
    extra
}

fn detached(d: &Value, route: &str, o: &Overrides) -> Result<super::RouteShape, String> {
    infer(
        d,
        &TypeTable::builtin(),
        &Extractors::builtin(),
        "bench::route_marker",
        route,
        o,
    )
}

/// `(path parameters, query parameters)` of a shape, in emitted order.
fn split(s: &super::RouteShape) -> (Vec<&super::route_sig::Parameter>, Vec<&str>) {
    let (paths, queries): (Vec<_>, Vec<_>) =
        s.parameters.iter().partition(|p| p.location == "path");
    (
        paths,
        queries.into_iter().map(|p| p.name.as_str()).collect(),
    )
}

#[test]
fn a_params_field_naming_a_placeholder_types_that_path_parameter() {
    let d = doc_types_only(sector_params("Filter", "sector"));
    let o = Overrides {
        params: Some("Filter".into()),
        ..Default::default()
    };
    let s = detached(&d, "/sectors/{sector}", &o).expect("detached shape builds");
    let (paths, queries) = split(&s);
    assert_eq!(queries, ["fields"], "the matched field left the query set");
    assert_eq!(paths.len(), 1, "got {:?}", s.parameters);
    assert_eq!(paths[0].name, "sector");
    assert!(paths[0].required);
    assert_eq!(paths[0].schema["$ref"], "#/components/schemas/Sector");
    assert_eq!(s.components["Sector"]["enum"], json!(["Tech", "Energy"]));
}

#[test]
fn a_placeholder_no_params_field_names_stays_an_open_string() {
    let d = doc_types_only(sector_params("Filter", "sector"));
    let o = Overrides {
        params: Some("Filter".into()),
        ..Default::default()
    };
    // `{industry}` matches nothing, so `sector` is only a query parameter.
    let s = detached(&d, "/industries/{industry}", &o).expect("detached shape builds");
    let (paths, queries) = split(&s);
    assert_eq!(queries, ["fields", "sector"]);
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].name, "industry");
    assert_eq!(paths[0].schema, json!({"type": "string"}));
}

#[test]
fn a_rust_keyword_field_names_the_placeholder_it_had_to_escape() {
    for spelling in ["type_", "r#type"] {
        let d = doc_types_only(sector_params("Filter", spelling));
        let o = Overrides {
            params: Some("Filter".into()),
            ..Default::default()
        };
        let s = detached(&d, "/holders/{symbol}/{type}", &o).expect("detached shape builds");
        let (paths, queries) = split(&s);
        assert_eq!(queries, ["fields"], "{spelling} left the query set");
        let names: Vec<&str> = paths.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["symbol", "type"], "{spelling}");
        assert_eq!(paths[0].schema, json!({"type": "string"}), "{spelling}");
        assert_eq!(
            paths[1].schema["$ref"], "#/components/schemas/Sector",
            "{spelling} typed `{{type}}`"
        );
    }
}

#[test]
fn path_parameters_follow_the_template_not_the_struct_field_order() {
    let mut extra = vec![
        (50, struct_item("Filter", &[51, 52], &[])),
        // Sorted field order is `alpha`, `zulu` — the reverse of the route's.
        (51, field("alpha", path("Sector", 60, &[]))),
        (52, field("zulu", prim("u32"))),
    ];
    extra.extend(unit_enum("Sector", 60, &["Tech"]));
    let d = doc_types_only(extra);
    let o = Overrides {
        params: Some("Filter".into()),
        ..Default::default()
    };
    let s = detached(&d, "/x/{zulu}/y/{alpha}/z/{omega}", &o).expect("detached shape builds");
    let (paths, queries) = split(&s);
    assert!(queries.is_empty(), "both fields named placeholders");
    let names: Vec<&str> = paths.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["zulu", "alpha", "omega"]);
    assert_eq!(paths[0].schema["format"], "int32");
    assert_eq!(paths[1].schema["$ref"], "#/components/schemas/Sector");
    assert_eq!(paths[2].schema, json!({"type": "string"}));
}

#[test]
fn path_params_bindings_type_placeholders_without_a_params_struct() {
    let d = doc_types_only(unit_enum("Sector", 60, &["Tech", "Energy"]));
    let o = Overrides {
        path_params: Some("sector: Sector".into()),
        ..Default::default()
    };
    let s = detached(&d, "/sectors/{sector}/items/{id}", &o)
        .expect("path_params alone is a detached contract");
    let (paths, _) = split(&s);
    let names: Vec<&str> = paths.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["sector", "id"]);
    assert_eq!(paths[0].schema["$ref"], "#/components/schemas/Sector");
    assert_eq!(
        paths[1].schema,
        json!({"type": "string"}),
        "unnamed stays open"
    );
}

#[test]
fn a_path_params_binding_displaces_the_params_field_that_named_it() {
    let mut extra = sector_params("Filter", "sector");
    extra.extend(unit_enum("Region", 70, &["Us", "Eu"]));
    let d = doc_types_only(extra);
    let o = Overrides {
        params: Some("Filter".into()),
        path_params: Some("sector: Region".into()),
        ..Default::default()
    };
    let s = detached(&d, "/sectors/{sector}", &o).expect("detached shape builds");
    let (paths, queries) = split(&s);
    assert_eq!(queries, ["fields"], "the field still left the query set");
    assert_eq!(paths[0].schema["$ref"], "#/components/schemas/Region");
}

#[test]
fn a_path_params_struct_types_placeholders_by_field_name() {
    let d = doc_types_only(sector_params("SectorPath", "sector"));
    let o = Overrides {
        path_params: Some("SectorPath".into()),
        ..Default::default()
    };
    let s = detached(&d, "/sectors/{sector}", &o).expect("detached shape builds");
    let (paths, queries) = split(&s);
    assert!(queries.is_empty(), "a path struct adds no query parameters");
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].schema["$ref"], "#/components/schemas/Sector");
}

#[test]
fn path_params_naming_an_absent_placeholder_is_an_error_not_a_silent_string() {
    let d = doc_types_only(unit_enum("Sector", 60, &["Tech"]));
    let o = Overrides {
        path_params: Some("secotr: Sector".into()),
        ..Default::default()
    };
    let err = detached(&d, "/sectors/{sector}", &o).expect_err("typo is caught");
    assert!(err.contains("`secotr`"), "names the typo: {err}");
}

#[test]
fn a_path_params_type_that_is_not_in_the_rustdoc_json_errors() {
    let d = doc_types_only(unit_enum("Sector", 60, &["Tech"]));
    let o = Overrides {
        path_params: Some("sector: Nope".into()),
        ..Default::default()
    };
    let err = detached(&d, "/sectors/{sector}", &o).expect_err("typo'd type is caught");
    assert!(err.contains("`Nope`"), "names the type: {err}");
}

#[test]
fn path_params_overrides_what_a_path_extractor_already_typed() {
    let d = doc(
        vec![("p", path("Path", 5, &[prim("u32")]))],
        Value::Null,
        unit_enum("Sector", 60, &["Tech"]),
    );
    let o = Overrides {
        path_params: Some("id: Sector".into()),
        ..Default::default()
    };
    let s = run(&d, "/items/{id}", &o);
    assert_eq!(s.parameters.len(), 1);
    assert_eq!(s.parameters[0].name, "id");
    assert_eq!(
        s.parameters[0].schema["$ref"],
        "#/components/schemas/Sector"
    );
}

#[test]
fn an_array_response_override_wraps_the_named_type() {
    let d = doc_types_only(item_type());
    let o = Overrides {
        response: Some("[Item]".into()),
        ..Default::default()
    };
    let s = infer(
        &d,
        &TypeTable::builtin(),
        &Extractors::builtin(),
        "bench::route_list_items",
        "/items",
        &o,
    )
    .expect("array override resolves");
    let schema = &s.responses["200"].schema;
    assert_eq!(schema["type"], "array");
    assert_eq!(schema["items"]["$ref"], "#/components/schemas/Item");
}
