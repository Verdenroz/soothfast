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
