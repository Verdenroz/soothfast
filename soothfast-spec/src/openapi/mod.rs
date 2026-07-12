//! OpenAPI 3.1 documents assembled from inferred route shapes.
//!
//! 3.1 rather than 3.0 because the extractor emits JSON Schema 2020-12
//! constructs (`const` for enum tags, `prefixItems` for tuples) that 3.0
//! has no way to express.

pub mod diff;
#[cfg(test)]
mod diff_tests;

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::dialect::{Document, Info, Operation, unknown_method};

/// The verbs a path item may key an operation by.
const HTTP_METHODS: &[&str] = &[
    "GET", "PUT", "POST", "DELETE", "OPTIONS", "HEAD", "PATCH", "TRACE",
];

/// Assemble an OpenAPI 3.1 document.
pub fn document(info: &Info, ops: &[Operation]) -> Document {
    let mut conflicts = Vec::new();
    let mut paths: BTreeMap<String, serde_json::Map<String, Value>> = BTreeMap::new();
    let mut components: BTreeMap<String, Value> = BTreeMap::new();

    for op in ops {
        if !HTTP_METHODS.contains(&op.method.to_ascii_uppercase().as_str()) {
            conflicts.push(unknown_method(op, "OpenAPI", "GET, POST, PUT, DELETE, ..."));
            continue;
        }
        for (name, schema) in &op.shape.components {
            if let Some(existing) = components.get(name)
                && existing != schema
            {
                conflicts.push(format!(
                    "component `{name}` has two different definitions \
                         (second seen via operation `{}`)",
                    op.operation_id
                ));
                continue;
            }
            components.insert(name.clone(), schema.clone());
        }

        let verb = op.method.to_ascii_lowercase();
        let item = paths.entry(op.path.clone()).or_default();
        if item.contains_key(&verb) {
            conflicts.push(format!(
                "{} {} is declared by more than one operation (`{}`)",
                op.method, op.path, op.operation_id
            ));
            continue;
        }
        item.insert(verb, operation_object(op));
    }

    let mut doc = serde_json::Map::new();
    doc.insert("openapi".into(), json!("3.1.0"));
    doc.insert("info".into(), info_object(info));
    if !info.servers.is_empty() {
        let servers: Vec<Value> = info.servers.iter().map(|u| json!({ "url": u })).collect();
        doc.insert("servers".into(), json!(servers));
    }
    let path_items: serde_json::Map<String, Value> = paths
        .into_iter()
        .map(|(p, item)| (p, Value::Object(item)))
        .collect();
    doc.insert("paths".into(), Value::Object(path_items));
    if !components.is_empty() {
        let schemas: serde_json::Map<String, Value> = components.into_iter().collect();
        doc.insert("components".into(), json!({ "schemas": schemas }));
    }

    Document {
        value: Value::Object(doc),
        conflicts,
        notes: Vec::new(),
    }
}

fn info_object(info: &Info) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("title".into(), json!(info.title));
    m.insert("version".into(), json!(info.version));
    if let Some(d) = &info.description {
        m.insert("description".into(), json!(d));
    }
    Value::Object(m)
}

fn operation_object(op: &Operation) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("operationId".into(), json!(op.operation_id));
    if let Some(s) = &op.summary {
        m.insert("summary".into(), json!(s));
    }

    if !op.shape.parameters.is_empty() {
        let params: Vec<Value> = op
            .shape
            .parameters
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "in": p.location,
                    "required": p.required,
                    "schema": p.schema,
                })
            })
            .collect();
        m.insert("parameters".into(), json!(params));
    }

    if let Some(body) = &op.shape.request {
        m.insert(
            "requestBody".into(),
            json!({
                "required": body.required,
                "content": { body.content_type.clone(): { "schema": body.schema } },
            }),
        );
    }

    let mut responses = serde_json::Map::new();
    for (code, resp) in &op.shape.responses {
        let mut r = serde_json::Map::new();
        // OpenAPI requires a description on every response object.
        let description = resp
            .description
            .clone()
            .unwrap_or_else(|| describe_status(code).to_string());
        r.insert("description".into(), json!(description));
        if !resp.headers.is_empty() {
            let headers: serde_json::Map<String, Value> = resp
                .headers
                .iter()
                .map(|h| (h.clone(), json!({ "schema": { "type": "string" } })))
                .collect();
            r.insert("headers".into(), Value::Object(headers));
        }
        if !resp.content_type.is_empty() {
            r.insert(
                "content".into(),
                json!({ resp.content_type.clone(): { "schema": resp.schema } }),
            );
        }
        responses.insert(code.clone(), Value::Object(r));
    }
    if responses.is_empty() {
        responses.insert("200".into(), json!({ "description": "OK" }));
    }
    m.insert("responses".into(), Value::Object(responses));

    Value::Object(m)
}

/// The description OpenAPI insists on, from the status code itself.
fn describe_status(code: &str) -> &'static str {
    match code {
        "200" => "OK",
        "201" => "Created",
        "202" => "Accepted",
        "204" => "No Content",
        "301" => "Moved Permanently",
        "302" => "Found",
        "304" => "Not Modified",
        "400" => "Bad Request",
        "401" => "Unauthorized",
        "403" => "Forbidden",
        "404" => "Not Found",
        "409" => "Conflict",
        "422" => "Unprocessable Content",
        "429" => "Too Many Requests",
        "500" => "Internal Server Error",
        "503" => "Service Unavailable",
        "default" => "Unexpected error",
        _ => "Response",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::route_sig::{Parameter, RequestBody, Response, RouteShape};
    use crate::serialize::to_yaml;

    fn info() -> Info {
        Info {
            title: "Items API".into(),
            version: "2.1".into(),
            description: None,
            servers: vec!["https://api.example.com".into()],
        }
    }

    fn shape() -> RouteShape {
        RouteShape::default()
    }

    fn op(operation_id: &str, method: &str, path: &str, shape: RouteShape) -> Operation {
        Operation {
            operation_id: operation_id.into(),
            method: method.into(),
            path: path.into(),
            summary: None,
            shape,
        }
    }

    fn json_response(schema: Value) -> Response {
        Response::json(schema)
    }

    #[test]
    fn emits_a_three_one_document_with_info_and_servers() {
        let d = document(&info(), &[]);
        assert_eq!(d.value["openapi"], "3.1.0");
        assert_eq!(d.value["info"]["title"], "Items API");
        assert_eq!(d.value["info"]["version"], "2.1");
        assert_eq!(d.value["servers"][0]["url"], "https://api.example.com");
        assert!(d.conflicts.is_empty());
    }

    #[test]
    fn the_verb_becomes_a_lowercased_path_item_key() {
        let d = document(&info(), &[op("createItem", "POST", "/items", shape())]);
        assert_eq!(
            d.value["paths"]["/items"]["post"]["operationId"],
            "createItem"
        );
    }

    #[test]
    fn two_verbs_on_one_path_merge_into_a_single_path_item() {
        let d = document(
            &info(),
            &[
                op("listItems", "GET", "/items", shape()),
                op("createItem", "POST", "/items", shape()),
            ],
        );
        let item = &d.value["paths"]["/items"];
        assert_eq!(item["get"]["operationId"], "listItems");
        assert_eq!(item["post"]["operationId"], "createItem");
        assert!(d.conflicts.is_empty());
    }

    #[test]
    fn a_duplicated_verb_and_path_is_a_conflict_not_a_silent_overwrite() {
        let d = document(
            &info(),
            &[
                op("listItems", "GET", "/items", shape()),
                op("otherList", "GET", "/items", shape()),
            ],
        );
        assert_eq!(d.conflicts.len(), 1, "got {:?}", d.conflicts);
        // The first declaration stands rather than being replaced.
        assert_eq!(
            d.value["paths"]["/items"]["get"]["operationId"],
            "listItems"
        );
    }

    #[test]
    fn a_method_from_another_dialect_is_a_conflict() {
        let d = document(
            &info(),
            &[op("streamPrices", "SUBSCRIBE", "prices", shape())],
        );
        assert_eq!(d.conflicts.len(), 1, "got {:?}", d.conflicts);
        assert!(d.conflicts[0].contains("SUBSCRIBE"), "{:?}", d.conflicts);
        assert!(
            d.value["paths"].as_object().is_some_and(|p| p.is_empty()),
            "the operation is not emitted under a guessed verb"
        );
    }

    #[test]
    fn components_shared_between_operations_merge_cleanly() {
        let mut a = shape();
        a.components
            .insert("Item".into(), json!({ "type": "object" }));
        let mut b = shape();
        b.components
            .insert("Item".into(), json!({ "type": "object" }));
        let d = document(&info(), &[op("a", "GET", "/a", a), op("b", "GET", "/b", b)]);
        assert!(d.conflicts.is_empty(), "identical definitions agree");
        assert_eq!(d.value["components"]["schemas"]["Item"]["type"], "object");
    }

    #[test]
    fn components_that_disagree_are_reported() {
        let mut a = shape();
        a.components
            .insert("Item".into(), json!({ "type": "object" }));
        let mut b = shape();
        b.components
            .insert("Item".into(), json!({ "type": "string" }));
        let d = document(&info(), &[op("a", "GET", "/a", a), op("b", "GET", "/b", b)]);
        assert_eq!(d.conflicts.len(), 1, "got {:?}", d.conflicts);
        assert!(d.conflicts[0].contains("Item"));
    }

    #[test]
    fn every_response_carries_the_description_openapi_requires() {
        let mut s = shape();
        s.responses
            .insert("201".into(), json_response(json!({ "type": "object" })));
        s.responses
            .insert("default".into(), json_response(json!({})));
        let d = document(&info(), &[op("createItem", "POST", "/items", s)]);
        let responses = &d.value["paths"]["/items"]["post"]["responses"];
        assert_eq!(responses["201"]["description"], "Created");
        assert_eq!(responses["default"]["description"], "Unexpected error");
    }

    #[test]
    fn a_no_content_response_has_no_content_block() {
        let mut s = shape();
        s.responses.insert("204".into(), Response::empty());
        let d = document(&info(), &[op("deleteItem", "DELETE", "/items/{id}", s)]);
        let r = &d.value["paths"]["/items/{id}"]["delete"]["responses"]["204"];
        assert_eq!(r["description"], "No Content");
        assert!(r.get("content").is_none(), "204 carries no body");
    }

    #[test]
    fn an_operation_with_no_responses_still_emits_one() {
        let d = document(&info(), &[op("ping", "GET", "/ping", shape())]);
        assert_eq!(
            d.value["paths"]["/ping"]["get"]["responses"]["200"]["description"],
            "OK"
        );
    }

    #[test]
    fn request_body_uses_the_content_type_the_extractor_implied() {
        let mut s = shape();
        s.request = Some(RequestBody {
            content_type: "application/x-www-form-urlencoded".into(),
            schema: json!({ "$ref": "#/components/schemas/NewItem" }),
            required: true,
        });
        let d = document(&info(), &[op("createItem", "POST", "/items", s)]);
        let body = &d.value["paths"]["/items"]["post"]["requestBody"];
        assert_eq!(body["required"], true);
        assert_eq!(
            body["content"]["application/x-www-form-urlencoded"]["schema"]["$ref"],
            "#/components/schemas/NewItem"
        );
    }

    #[test]
    fn parameters_render_with_location_and_requiredness() {
        let mut s = shape();
        s.parameters.push(Parameter {
            name: "id".into(),
            location: "path".into(),
            required: true,
            schema: json!({ "type": "integer" }),
        });
        s.parameters.push(Parameter {
            name: "verbose".into(),
            location: "query".into(),
            required: false,
            schema: json!({ "type": "boolean" }),
        });
        let d = document(&info(), &[op("getItem", "GET", "/items/{id}", s)]);
        let params = d.value["paths"]["/items/{id}"]["get"]["parameters"]
            .as_array()
            .expect("parameters");
        assert_eq!(params[0]["name"], "id");
        assert_eq!(params[0]["in"], "path");
        assert_eq!(params[0]["required"], true);
        assert_eq!(params[1]["required"], false);
    }

    #[test]
    fn yaml_output_is_stable_and_leads_with_the_version() {
        let mut s = shape();
        s.responses
            .insert("200".into(), json_response(json!({ "type": "object" })));
        s.components
            .insert("Item".into(), json!({ "type": "object" }));
        let d = document(&info(), &[op("getItem", "GET", "/items/{id}", s)]);
        let a = to_yaml(&d.value);
        let b = to_yaml(&d.value);
        assert_eq!(a, b, "regeneration must be byte-identical");
        assert!(a.starts_with("openapi: 3.1.0"), "got:\n{a}");
        assert!(a.ends_with('\n'), "file ends with a newline");
    }
}
