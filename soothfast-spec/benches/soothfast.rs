//! The spec engine measured by soothfast: `spec gen` assembles a document
//! from every route in a crate, and `spec gate` diffs that document against
//! the merge-base — both on every CI run that touches an annotated crate.

use serde_json::json;
use soothfast::{bench, fixture, keep};
use soothfast_spec::dialect::{Info, Operation};
use soothfast_spec::schema::RouteShape;
use soothfast_spec::schema::route_sig::{Parameter, Response};

soothfast::bench_main!();

fn info() -> Info {
    Info {
        title: "Bench API".into(),
        version: "0.1.0".into(),
        description: None,
        servers: vec!["https://api.example.test".into()],
    }
}

/// `n` routes, each with parameters, a body and a component schema — the
/// shape a real annotated crate presents to the emitters.
#[fixture]
fn ops_n(n: usize) -> Vec<Operation> {
    (0..n)
        .map(|i| {
            let mut shape = RouteShape::default();
            shape.parameters.push(Parameter {
                name: format!("id_{i}"),
                location: "path".into(),
                required: true,
                schema: json!({ "type": "string" }),
            });
            shape.parameters.push(Parameter {
                name: "limit".into(),
                location: "query".into(),
                required: false,
                schema: json!({ "type": "integer" }),
            });
            shape.components.insert(
                format!("Item{i}"),
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer" },
                        "name": { "type": "string" },
                        "tags": { "type": "array", "items": { "type": "string" } },
                    },
                    "required": ["id"],
                }),
            );
            shape.responses.insert(
                "200".into(),
                Response::json(json!({ "$ref": format!("#/components/schemas/Item{i}") })),
            );
            Operation {
                operation_id: format!("get_item_{i}"),
                method: "GET".into(),
                path: format!("/v1/items/{{id_{i}}}"),
                summary: Some("Fetch one item.".into()),
                shape,
            }
        })
        .collect()
}

/// Assembling the OpenAPI document: what `spec gen` spends its time on
/// once rustdoc JSON is in hand.
#[bench(
    group = "self",
    setup_sized = ops_n,
    sizes(16, 64, 256),
    complexity = "n",
    covers = "soothfast_spec::openapi::document"
)]
fn bench_openapi_document(ops: &[Operation]) {
    keep(soothfast_spec::openapi::document(&info(), keep(ops)));
}

/// The consumer-compatibility diff `spec gate` runs on every pull request.
/// Both sides are the same document, which is the gate's ordinary case —
/// the work is the walk, not the number of findings.
#[bench(
    group = "self",
    setup_sized = ops_n,
    sizes(16, 64, 256),
    complexity = "n",
    covers = "soothfast_spec::openapi::diff::diff"
)]
fn bench_openapi_diff(ops: &[Operation]) {
    let doc = soothfast_spec::openapi::document(&info(), ops);
    keep(soothfast_spec::openapi::diff::diff(
        keep(&doc.value),
        keep(&doc.value),
    ));
}

/// Rendering the document out with a fixed key order — every `spec gen`
/// writes one of these per dialect.
#[bench(
    group = "self",
    setup_sized = ops_n,
    sizes(16, 64, 256),
    complexity = "n",
    covers = "soothfast_spec::serialize::to_yaml"
)]
fn bench_serialize_yaml(ops: &[Operation]) {
    let doc = soothfast_spec::openapi::document(&info(), ops);
    keep(soothfast_spec::serialize::to_yaml(keep(&doc.value)));
}
