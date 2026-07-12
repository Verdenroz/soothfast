//! Emitter variants the golden fixture does not exercise: required query
//! parameters, pagination without other filters, awkward wire names, and an
//! SDK with no component models at all.
//!
//! Set `DUMP_VARIANTS=<dir>` to write each variant's file tree out for a
//! real `tsc` run.

use std::collections::BTreeMap;

use serde_json::json;
use soothfast_sdk::{SdkFileSet, SdkKind, SdkOptions};
use soothfast_spec::dialect::{Info, Operation};
use soothfast_spec::schema::RouteShape;
use soothfast_spec::schema::route_sig::{Parameter, RequestBody, Response};

fn param(name: &str, location: &str, required: bool, schema: serde_json::Value) -> Parameter {
    Parameter {
        name: name.into(),
        location: location.into(),
        required,
        schema,
    }
}

fn info() -> Info {
    Info {
        title: "Variants API".into(),
        version: "0.1.0".into(),
        description: None,
        servers: vec![],
    }
}

fn options() -> SdkOptions {
    SdkOptions {
        package: "variants".into(),
        module: "variants".into(),
        version: "0.1.0".into(),
        base_url: Some("https://api.example.test".into()),
        ..SdkOptions::default()
    }
}

/// A query parameter with no default forces the options object to be
/// required rather than defaulted.
fn required_query_op() -> Operation {
    let mut shape = RouteShape::default();
    shape
        .parameters
        .push(param("q", "query", true, json!({ "type": "string" })));
    shape
        .parameters
        .push(param("page", "query", false, json!({ "type": "integer" })));
    shape.responses.insert(
        "200".into(),
        Response::json(json!({ "type": "array", "items": { "type": "string" } })),
    );
    Operation {
        operation_id: "search".into(),
        method: "GET".into(),
        path: "/search".into(),
        summary: Some("Search.".into()),
        shape,
    }
}

/// Paginated with nothing else to filter by: the iter options interface has
/// no base to extend.
fn bare_pagination_op() -> Operation {
    let mut shape = RouteShape::default();
    shape.parameters.push(param(
        "page[size]",
        "query",
        false,
        json!({ "type": "integer" }),
    ));
    shape.parameters.push(param(
        "page[after]",
        "query",
        false,
        json!({ "type": "string" }),
    ));
    shape.responses.insert(
        "200".into(),
        Response::json(json!({ "type": "array", "items": { "type": "number" } })),
    );
    Operation {
        operation_id: "list-events".into(),
        method: "GET".into(),
        path: "/events".into(),
        summary: None,
        shape,
    }
}

/// Reserved words and punctuation in every position that has to become an
/// identifier, plus a header parameter (which no emitter surfaces).
fn awkward_names_op() -> Operation {
    let mut shape = RouteShape::default();
    shape
        .parameters
        .push(param("new", "path", true, json!({ "type": "string" })));
    shape.parameters.push(param(
        "Retry-After",
        "query",
        false,
        json!({ "type": "string" }),
    ));
    shape.parameters.push(param(
        "X-Trace",
        "header",
        false,
        json!({ "type": "string" }),
    ));
    shape.responses.insert("204".into(), Response::empty());
    Operation {
        operation_id: "delete".into(),
        method: "DELETE".into(),
        path: "/things/{new}".into(),
        summary: Some("Ends the */ comment early.".into()),
        shape,
    }
}

fn variants() -> Vec<(&'static str, Vec<Operation>, SdkOptions)> {
    vec![
        ("required-query", vec![required_query_op()], options()),
        (
            "bare-pagination",
            vec![bare_pagination_op()],
            SdkOptions {
                paginated: vec!["list-events".into()],
                limit_param: "page[size]".into(),
                cursor_param: "page[after]".into(),
                ..options()
            },
        ),
        ("awkward-names", vec![awkward_names_op()], options()),
    ]
}

fn emit(ops: &[Operation], opts: &SdkOptions) -> SdkFileSet {
    SdkKind::TypeScript.emit(&info(), ops, opts).expect("emits")
}

/// Path parameters named after the arguments the emitters give themselves.
fn reserved_names_op() -> Operation {
    let mut shape = RouteShape::default();
    shape
        .parameters
        .push(param("self", "path", true, json!({ "type": "string" })));
    shape
        .parameters
        .push(param("body", "path", true, json!({ "type": "string" })));
    shape.request = Some(RequestBody {
        content_type: "application/json".into(),
        schema: json!({ "type": "object" }),
        required: true,
    });
    shape
        .responses
        .insert("200".into(), Response::json(json!({ "type": "object" })));
    Operation {
        operation_id: "stash".into(),
        method: "POST".into(),
        path: "/things/{self}/{body}".into(),
        summary: None,
        shape,
    }
}

/// A parameter named like the client's own argument has to move, or the
/// emitted file declares the same name twice and will not even parse.
#[test]
fn a_parameter_cannot_shadow_the_clients_own_argument() {
    let py = SdkKind::Python
        .emit(&info(), &[reserved_names_op()], &options())
        .expect("emits");
    let client = &py.files["src/variants/client.py"];
    assert!(client.contains("self_: str,"), "{client}");
    assert!(client.contains("body_: str,"), "{client}");
    // The request body keeps `body`; the path parameter is what moved.
    assert!(client.contains("body: dict["), "{client}");
    assert!(
        client.contains("f\"/things/{path_seg(self_)}/{path_seg(body_)}\""),
        "{client}"
    );
    assert!(
        py.notes.iter().any(|n| n.contains("`self`")),
        "{:?}",
        py.notes
    );

    let ts = emit(&[reserved_names_op()], &options());
    let client = &ts.files["src/client.ts"];
    // TypeScript only has to move `body`; `self` is an ordinary binding.
    assert!(client.contains("self: string"), "{client}");
    assert!(client.contains("body_: string"), "{client}");
    assert!(client.contains("${pathSeg(body_)}"), "{client}");
}

#[test]
fn a_required_query_parameter_makes_the_options_object_required() {
    let files = emit(&[required_query_op()], &options()).files;
    let client = &files["src/client.ts"];
    assert!(
        client.contains("search(options: SearchOptions): Promise"),
        "{client}"
    );
    assert!(client.contains("  q: string;"), "{client}");
    assert!(client.contains("  page?: number;"), "{client}");
}

/// A defaulted page size is not enough to default the whole object: the
/// paginated variant of a search still has to be given something to search
/// for, and `iterSearch()` would not compile under `strict`.
#[test]
fn a_required_filter_survives_into_the_paginated_variant() {
    let opts = SdkOptions {
        paginated: vec!["search".into()],
        ..options()
    };
    let files = emit(&[required_query_op()], &opts).files;
    let client = &files["src/client.ts"];
    assert!(
        client.contains("iterSearch(options: SearchIterOptions): AsyncPager"),
        "{client}"
    );
    assert!(
        !client.contains("iterSearch(options: SearchIterOptions = {}"),
        "a required filter cannot default to an empty object: {client}"
    );
}

#[test]
fn pagination_without_filters_needs_no_base_options_interface() {
    let (_, ops, opts) = variants().remove(1);
    let files = emit(&ops, &opts).files;
    let client = &files["src/client.ts"];
    assert!(
        !client.contains("interface ListEventsOptions"),
        "nothing to put in it: {client}"
    );
    assert!(
        client.contains("export interface ListEventsIterOptions {"),
        "{client}"
    );
    // Unquotable pagination keys survive into both the destructuring and
    // the query literal.
    assert!(
        client.contains("const { \"page[size]\": pageLimit = 50, ...rest } = options;"),
        "{client}"
    );
    assert!(
        client.contains("\"page[size]\": pageLimit, \"page[after]\": cursor"),
        "{client}"
    );
}

#[test]
fn reserved_words_become_legal_bindings_and_headers_stay_out_of_the_surface() {
    let files = emit(&[awkward_names_op()], &options()).files;
    let client = &files["src/client.ts"];
    // `delete` is legal as a method name; `new` is not legal as a binding.
    assert!(client.contains("delete(new_: string"), "{client}");
    assert!(client.contains("${pathSeg(new_)}"), "{client}");
    assert!(client.contains("\"Retry-After\"?: string;"), "{client}");
    assert!(
        !client.contains("X-Trace"),
        "headers are not surfaced: {client}"
    );
    // A doc comment may not close its own block.
    assert!(client.contains("Ends the *\\/ comment early."), "{client}");
}

#[test]
fn an_sdk_with_no_components_still_emits_a_module() {
    let files = emit(&[required_query_op()], &options()).files;
    assert_eq!(
        files["src/models.ts"].trim_end().lines().last(),
        Some("export {};")
    );
    assert!(
        !files["src/index.ts"].contains("./models.js"),
        "nothing to re-export: {}",
        files["src/index.ts"]
    );
    assert!(
        !files["src/client.ts"].contains("import type * as models"),
        "unused import would fail a strict build"
    );
}

/// Writes every variant out so `tsc` can be pointed at it. Off by default:
/// the assertions above are what CI runs.
#[test]
fn dump_variants_for_a_real_typescript_build() {
    let Some(root) = std::env::var_os("DUMP_VARIANTS") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    for (name, ops, opts) in variants() {
        let dir = root.join(name);
        let _ = std::fs::remove_dir_all(&dir);
        let files: BTreeMap<String, String> = emit(&ops, &opts).files;
        for (rel, content) in &files {
            let target = dir.join(rel);
            std::fs::create_dir_all(target.parent().expect("parent")).expect("mkdir");
            std::fs::write(&target, content).expect("write");
        }
    }
}
