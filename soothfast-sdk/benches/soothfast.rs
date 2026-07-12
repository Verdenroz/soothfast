//! The SDK engine measured by soothfast: `sdk gen` lowers every operation
//! into the language-neutral model, then renders a whole package from it.

use serde_json::json;
use soothfast::{bench, fixture, keep};
use soothfast_sdk::{SdkKind, SdkOptions};
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

fn options() -> SdkOptions {
    SdkOptions {
        package: "bench-client".into(),
        module: "bench_client".into(),
        version: "0.1.0".into(),
        base_url: Some("https://api.example.test".into()),
        ..SdkOptions::default()
    }
}

/// `n` routes carrying the shapes the lowerer has to decide about: a
/// component model, a union, and both parameter locations.
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
                name: "logoUrl".into(),
                location: "query".into(),
                required: false,
                schema: json!({ "type": "string" }),
            });
            shape.components.insert(
                format!("Item{i}"),
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer" },
                        "name": { "type": "string" },
                        "either": { "oneOf": [{ "type": "string" }, { "type": "integer" }] },
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

/// Lowering: where every typing question is answered once, before either
/// emitter sees the model.
#[bench(
    group = "self",
    setup_sized = ops_n,
    sizes(16, 64, 256),
    complexity = "n",
    covers = "soothfast_sdk::lower::lower"
)]
fn bench_lower(ops: &[Operation]) {
    keep(
        soothfast_sdk::lower::lower(keep(ops), &options(), SdkKind::Python)
            .expect("synthetic operations lower"),
    );
}

/// Emitting a whole TypeScript package — models, client, and the packaging
/// around them. What `sdk gen` costs per target.
///
/// No `covers`: the surface index carries free functions, and `emit` is a
/// method, so pointing at it would attribute to nothing at all.
#[bench(
    group = "self",
    setup_sized = ops_n,
    sizes(16, 64, 256),
    complexity = "n"
)]
fn bench_emit_typescript(ops: &[Operation]) {
    keep(
        SdkKind::TypeScript
            .emit(&info(), keep(ops), &options())
            .expect("synthetic operations emit"),
    );
}
