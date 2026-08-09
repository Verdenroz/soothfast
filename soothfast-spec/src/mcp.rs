//! MCP tool manifests generated from `#[route(method = "TOOL")]` handlers.
//!
//! A tool is a name, a description and an `inputSchema` — which is exactly
//! what the extractor already produces, so this dialect is mostly plumbing.
//! The one structural difference from OpenAPI is where shared schemas live:
//! each tool's schema is a self-contained JSON Schema document, so components
//! are inlined as `$defs` under the schema that references them rather than
//! collected at the document root.
//!
//! Both schemas are rooted at an object because MCP clients build a form (or
//! a tool-call argument object) from them. A response whose root is not an
//! object cannot be described as structured output, so `outputSchema` is
//! omitted and the shortfall reported — an omitted output schema means
//! "unstructured", which is true, where a wrapped one would be a contract
//! nobody serves.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::compat::{Change, Direction, SchemaDiff, Severity, compare_keys, sort};
use crate::dialect::{Document, Info, Operation, unknown_method};
use crate::schema::route_sig::RouteShape;

/// Where the extractor puts shared schemas, and where MCP expects them.
const COMPONENTS_PREFIX: &str = "#/components/schemas/";
const DEFS_PREFIX: &str = "#/$defs/";

/// Assemble an MCP tool manifest.
///
/// `info` is unused: a tool manifest has no document-level metadata to carry,
/// and inventing a wrapper object for it would break every client that reads
/// the standard `{ "tools": [...] }` shape.
pub fn document(_info: &Info, ops: &[Operation]) -> Document {
    let mut doc = Document::default();
    let mut tools: BTreeMap<String, Value> = BTreeMap::new();

    for op in ops {
        if !op.method.eq_ignore_ascii_case("TOOL") {
            doc.conflicts.push(unknown_method(op, "MCP", "TOOL"));
            continue;
        }
        if tools.contains_key(&op.operation_id) {
            doc.conflicts.push(format!(
                "tool `{}` is declared by more than one route",
                op.operation_id
            ));
            continue;
        }
        tools.insert(op.operation_id.clone(), tool_object(op, &mut doc));
    }

    // Tools emit as an array (the shape clients read) but are built in a map,
    // so the manifest is ordered by name rather than by link order.
    doc.value = json!({ "tools": tools.into_values().collect::<Vec<_>>() });
    doc
}

fn tool_object(op: &Operation, doc: &mut Document) -> Value {
    let mut tool = serde_json::Map::new();
    tool.insert("name".into(), json!(op.operation_id));
    if let Some(s) = &op.summary {
        tool.insert("description".into(), json!(s));
    }
    tool.insert("inputSchema".into(), input_schema(op, doc));
    if let Some(out) = output_schema(op, doc) {
        tool.insert("outputSchema".into(), out);
    }
    Value::Object(tool)
}

/// The object a caller fills in: the request body's own fields, plus any
/// parameters the handler also takes.
fn input_schema(op: &Operation, doc: &mut Document) -> Value {
    let shape = &op.shape;
    let mut properties = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();

    if let Some(body) = &shape.request {
        let root = inline_root(&body.schema, &shape.components);
        if let Some(props) = root["properties"].as_object() {
            for (name, schema) in props {
                properties.insert(name.clone(), schema.clone());
            }
            required.extend(
                root["required"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(String::from)),
            );
        } else if !root.as_object().is_none_or(serde_json::Map::is_empty) {
            // A body that is not an object (an array, a bare string) has no
            // field names to offer as tool arguments.
            doc.notes.push(format!(
                "tool `{}`: the request body is not an object, so it cannot \
                 become named tool arguments; inputSchema is left open",
                op.operation_id
            ));
        }
    }

    for p in &shape.parameters {
        if properties.contains_key(&p.name) {
            doc.conflicts.push(format!(
                "tool `{}`: parameter `{}` collides with a request body field \
                 of the same name",
                op.operation_id, p.name
            ));
            continue;
        }
        properties.insert(p.name.clone(), rewrite_refs(&p.schema));
        if p.required {
            required.push(p.name.clone());
        }
    }

    let mut schema = json!({ "type": "object", "properties": properties });
    if !required.is_empty() {
        required.sort();
        required.dedup();
        schema["required"] = json!(required);
    }
    attach_defs(&mut schema, &shape.components);
    schema
}

/// The structured result a caller can rely on, when there is one.
fn output_schema(op: &Operation, doc: &mut Document) -> Option<Value> {
    let (_, response) = success_response(&op.shape)?;
    if response.schema.is_null() {
        return None;
    }
    let root = inline_root(&response.schema, &op.shape.components);
    if root["type"] != "object" {
        doc.notes.push(format!(
            "tool `{}`: the success response is not an object, which MCP \
             cannot describe as structured output; outputSchema is omitted \
             and the result is unstructured",
            op.operation_id
        ));
        return None;
    }
    let mut schema = root;
    attach_defs(&mut schema, &op.shape.components);
    Some(schema)
}

/// The success response: the lowest 2xx status the handler declares.
fn success_response(shape: &RouteShape) -> Option<(&String, &crate::schema::route_sig::Response)> {
    shape
        .responses
        .iter()
        .filter(|(code, _)| code.starts_with('2'))
        .min_by_key(|(code, _)| code.parse::<u16>().unwrap_or(u16::MAX))
}

/// Resolve a root `$ref` into the component it names, so the schema starts at
/// an object rather than a pointer. Nested refs are left alone: they become
/// `$defs` entries.
fn inline_root(schema: &Value, components: &BTreeMap<String, Value>) -> Value {
    let resolved = match component_name(schema) {
        Some(name) => components.get(&name).cloned().unwrap_or_else(|| json!({})),
        None => schema.clone(),
    };
    rewrite_refs(&resolved)
}

/// Attach every component the schema still references, transitively, under
/// `$defs`, and nothing else — an unreferenced definition in a tool manifest
/// is noise a client has to skip past.
fn attach_defs(schema: &mut Value, components: &BTreeMap<String, Value>) {
    let mut wanted: BTreeSet<String> = BTreeSet::new();
    let mut frontier: Vec<String> = referenced(schema).into_iter().collect();
    while let Some(name) = frontier.pop() {
        if !wanted.insert(name.clone()) {
            continue;
        }
        if let Some(def) = components.get(&name) {
            frontier.extend(referenced(&rewrite_refs(def)));
        }
    }
    if wanted.is_empty() {
        return;
    }
    let defs: serde_json::Map<String, Value> = wanted
        .into_iter()
        .map(|name| {
            let body = components
                .get(&name)
                .map(rewrite_refs)
                .unwrap_or_else(|| json!({}));
            (name, body)
        })
        .collect();
    schema["$defs"] = Value::Object(defs);
}

/// The component a `$ref` names, if it is one of ours.
fn component_name(schema: &Value) -> Option<String> {
    let r = schema.get("$ref")?.as_str()?;
    r.strip_prefix(COMPONENTS_PREFIX)
        .or_else(|| r.strip_prefix(DEFS_PREFIX))
        .map(String::from)
}

/// Repoint every `$ref` at `$defs`, where a self-contained JSON Schema
/// document keeps its definitions.
fn rewrite_refs(schema: &Value) -> Value {
    match schema {
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| {
                    if k == "$ref"
                        && let Some(name) =
                            v.as_str().and_then(|r| r.strip_prefix(COMPONENTS_PREFIX))
                    {
                        return (k.clone(), json!(format!("{DEFS_PREFIX}{name}")));
                    }
                    (k.clone(), rewrite_refs(v))
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(rewrite_refs).collect()),
        other => other.clone(),
    }
}

/// Every `$defs` name a schema refers to, one level deep.
fn referenced(schema: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_refs(schema, &mut out);
    out
}

fn collect_refs(schema: &Value, out: &mut BTreeSet<String>) {
    match schema {
        Value::Object(o) => {
            for (k, v) in o {
                // A `$defs` block already holds definitions, not references.
                if k == "$defs" {
                    continue;
                }
                if k == "$ref" {
                    if let Some(name) = v.as_str().and_then(|r| r.strip_prefix(DEFS_PREFIX)) {
                        out.insert(name.to_string());
                    }
                    continue;
                }
                collect_refs(v, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|v| collect_refs(v, out)),
        _ => {}
    }
}

/// Compare two generated tool manifests, oldest first.
///
/// A tool's arguments flow inward and its result flows outward, so the two
/// schemas are compared in opposite directions — the same asymmetry OpenAPI
/// has between a request body and a response.
pub fn diff(old: &Value, new: &Value) -> Vec<Change> {
    let mut changes = Vec::new();
    let (old_tools, new_tools) = (by_name(old), by_name(new));

    compare_keys(
        &mut changes,
        &old_tools,
        &new_tools,
        "tool",
        |ch, at, o, n| {
            let (oi, ni) = (&o["inputSchema"], &n["inputSchema"]);
            SchemaDiff::new(oi, ni).compare(ch, &format!("{at} input"), oi, ni, Direction::Request);

            match (o.get("outputSchema"), n.get("outputSchema")) {
                (Some(oo), Some(no)) => {
                    SchemaDiff::new(oo, no).compare(
                        ch,
                        &format!("{at} output"),
                        oo,
                        no,
                        Direction::Response,
                    );
                }
                // Structured output a client already parses cannot be withdrawn.
                (Some(_), None) => ch.push(Change::new(
                    Severity::Breaking,
                    format!("{at} output"),
                    "structured output removed",
                )),
                (None, Some(_)) => ch.push(Change::new(
                    Severity::Additive,
                    format!("{at} output"),
                    "structured output added",
                )),
                (None, None) => {}
            }
        },
    );

    sort(&mut changes);
    changes
}

fn by_name(doc: &Value) -> BTreeMap<String, Value> {
    doc["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["name"].as_str().map(|n| (n.to_string(), t.clone())))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::is_compatible;
    use crate::schema::route_sig::{Parameter, RequestBody, Response};

    fn info() -> Info {
        Info::default()
    }

    fn tool(name: &str, shape: RouteShape) -> Operation {
        Operation {
            operation_id: name.into(),
            method: "TOOL".into(),
            path: name.into(),
            summary: Some(format!("Does {name}.")),
            shape,
        }
    }

    fn object(props: Value, required: &[&str]) -> Value {
        json!({ "type": "object", "properties": props, "required": required })
    }

    #[test]
    fn a_tool_carries_its_name_and_doc_comment() {
        let d = document(&info(), &[tool("get_item", RouteShape::default())]);
        assert_eq!(d.value["tools"][0]["name"], "get_item");
        assert_eq!(d.value["tools"][0]["description"], "Does get_item.");
        assert_eq!(d.value["tools"][0]["inputSchema"]["type"], "object");
        assert!(d.conflicts.is_empty());
    }

    #[test]
    fn tools_emit_in_name_order_whatever_order_the_routes_registered() {
        let d = document(
            &info(),
            &[
                tool("zeta", RouteShape::default()),
                tool("alpha", RouteShape::default()),
            ],
        );
        let names: Vec<&str> = d.value["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
    }

    #[test]
    fn the_request_body_becomes_the_tools_arguments() {
        let mut shape = RouteShape::default();
        shape.components.insert(
            "NewItem".into(),
            object(
                json!({ "name": { "type": "string" }, "qty": { "type": "integer" } }),
                &["name"],
            ),
        );
        shape.request = Some(RequestBody {
            content_type: "application/json".into(),
            schema: json!({ "$ref": "#/components/schemas/NewItem" }),
            required: true,
        });
        let d = document(&info(), &[tool("create_item", shape)]);
        let input = &d.value["tools"][0]["inputSchema"];
        assert_eq!(input["type"], "object", "root is inlined, not a $ref");
        assert_eq!(input["properties"]["name"]["type"], "string");
        assert_eq!(input["required"][0], "name");
    }

    #[test]
    fn parameters_join_the_argument_object() {
        let mut shape = RouteShape::default();
        shape.parameters.push(Parameter {
            name: "id".into(),
            location: "path".into(),
            required: true,
            schema: json!({ "type": "integer" }),
        });
        shape.parameters.push(Parameter {
            name: "verbose".into(),
            location: "query".into(),
            required: false,
            schema: json!({ "type": "boolean" }),
        });
        let d = document(&info(), &[tool("get_item", shape)]);
        let input = &d.value["tools"][0]["inputSchema"];
        assert_eq!(input["properties"]["id"]["type"], "integer");
        assert_eq!(input["required"], json!(["id"]), "only the required one");
    }

    #[test]
    fn a_parameter_colliding_with_a_body_field_is_a_conflict() {
        let mut shape = RouteShape {
            request: Some(RequestBody {
                content_type: "application/json".into(),
                schema: object(json!({ "id": { "type": "string" } }), &[]),
                required: true,
            }),
            ..RouteShape::default()
        };
        shape.parameters.push(Parameter {
            name: "id".into(),
            location: "path".into(),
            required: true,
            schema: json!({ "type": "integer" }),
        });
        let d = document(&info(), &[tool("get_item", shape)]);
        assert_eq!(d.conflicts.len(), 1, "got {:?}", d.conflicts);
        assert!(d.conflicts[0].contains("collides"), "{:?}", d.conflicts);
    }

    #[test]
    fn referenced_components_travel_with_the_schema_as_defs() {
        let mut shape = RouteShape::default();
        shape.components.insert(
            "Item".into(),
            object(
                json!({ "tag": { "$ref": "#/components/schemas/Tag" } }),
                &[],
            ),
        );
        shape
            .components
            .insert("Tag".into(), json!({ "type": "string" }));
        shape.components.insert(
            "Unrelated".into(),
            json!({ "type": "object", "properties": {} }),
        );
        shape.responses.insert(
            "200".into(),
            Response::json(json!({ "$ref": "#/components/schemas/Item" })),
        );
        let d = document(&info(), &[tool("get_item", shape)]);
        let out = &d.value["tools"][0]["outputSchema"];
        assert_eq!(
            out["properties"]["tag"]["$ref"], "#/$defs/Tag",
            "nested refs are repointed at $defs"
        );
        let defs = out["$defs"].as_object().expect("$defs");
        assert!(defs.contains_key("Tag"));
        assert!(
            !defs.contains_key("Unrelated"),
            "only reachable definitions travel: {defs:?}"
        );
    }

    #[test]
    fn a_parameter_typed_by_a_component_becomes_a_self_contained_def() {
        // Regression: a request-body field's $ref was rewritten to `$defs`
        // via `inline_root`, but a parameter's own $ref went in unrewritten
        // — the tool's inputSchema pointed at `#/components/schemas/...`,
        // which does not exist anywhere in a standalone tool manifest.
        let mut shape = RouteShape::default();
        shape.components.insert(
            "AnalysisType".into(),
            json!({ "type": "string", "enum": ["a", "b"] }),
        );
        shape.parameters.push(Parameter {
            name: "analysis_type".into(),
            location: "query".into(),
            required: true,
            schema: json!({ "$ref": "#/components/schemas/AnalysisType" }),
        });
        let d = document(&info(), &[tool("get_analysis", shape)]);
        let input = &d.value["tools"][0]["inputSchema"];
        assert_eq!(
            input["properties"]["analysis_type"]["$ref"], "#/$defs/AnalysisType",
            "a parameter's $ref must be repointed at $defs like a body field's"
        );
        let defs = input["$defs"].as_object().expect("$defs");
        assert!(
            defs.contains_key("AnalysisType"),
            "the referenced component must travel with the schema: {defs:?}"
        );
    }

    #[test]
    fn a_non_object_response_omits_structured_output_and_says_why() {
        let mut shape = RouteShape::default();
        shape.responses.insert(
            "200".into(),
            Response::json(json!({ "type": "array", "items": { "type": "string" } })),
        );
        let d = document(&info(), &[tool("list_tags", shape)]);
        assert!(d.value["tools"][0].get("outputSchema").is_none());
        assert_eq!(d.notes.len(), 1, "got {:?}", d.notes);
        assert!(d.notes[0].contains("unstructured"), "{:?}", d.notes);
    }

    #[test]
    fn a_no_content_handler_has_no_output_schema() {
        let mut shape = RouteShape::default();
        shape.responses.insert("204".into(), Response::empty());
        let d = document(&info(), &[tool("delete_item", shape)]);
        assert!(d.value["tools"][0].get("outputSchema").is_none());
        assert!(d.notes.is_empty(), "no content is not a shortfall");
    }

    #[test]
    fn a_non_tool_method_is_a_conflict_not_a_guess() {
        let mut op = tool("get_item", RouteShape::default());
        op.method = "GET".into();
        let d = document(&info(), &[op]);
        assert_eq!(d.conflicts.len(), 1, "got {:?}", d.conflicts);
        assert!(d.value["tools"].as_array().is_some_and(Vec::is_empty));
    }

    fn manifest(input: Value, output: Option<Value>) -> Value {
        let mut t = json!({ "name": "create_item", "inputSchema": input });
        if let Some(o) = output {
            t["outputSchema"] = o;
        }
        json!({ "tools": [t] })
    }

    #[test]
    fn a_newly_required_argument_breaks_callers() {
        let old = manifest(object(json!({ "a": { "type": "string" } }), &[]), None);
        let new = manifest(
            object(
                json!({ "a": { "type": "string" }, "b": { "type": "string" } }),
                &["b"],
            ),
            None,
        );
        let changes = diff(&old, &new);
        assert!(!is_compatible(&changes), "got {changes:?}");
        assert_eq!(changes[0].severity, Severity::Breaking);
    }

    #[test]
    fn an_optional_new_argument_is_additive() {
        let old = manifest(object(json!({ "a": { "type": "string" } }), &[]), None);
        let new = manifest(
            object(
                json!({ "a": { "type": "string" }, "b": { "type": "string" } }),
                &[],
            ),
            None,
        );
        assert!(is_compatible(&diff(&old, &new)));
    }

    #[test]
    fn withdrawing_structured_output_breaks_readers() {
        let input = object(json!({}), &[]);
        let old = manifest(
            input.clone(),
            Some(object(json!({ "id": { "type": "string" } }), &["id"])),
        );
        let new = manifest(input, None);
        let changes = diff(&old, &new);
        assert!(!is_compatible(&changes), "got {changes:?}");
        assert!(changes[0].detail.contains("structured output removed"));
    }

    #[test]
    fn an_output_field_that_stops_being_guaranteed_breaks_readers() {
        let input = object(json!({}), &[]);
        let old = manifest(
            input.clone(),
            Some(object(json!({ "id": { "type": "string" } }), &["id"])),
        );
        let new = manifest(
            input,
            Some(object(json!({ "id": { "type": "string" } }), &[])),
        );
        let changes = diff(&old, &new);
        assert!(!is_compatible(&changes), "got {changes:?}");
        assert!(changes[0].detail.contains("no longer guaranteed"));
    }

    #[test]
    fn defs_resolve_per_tool_when_diffing() {
        let schema = json!({ "$ref": "#/$defs/Item",
                             "$defs": { "Item": { "type": "object",
                                                  "properties": { "id": { "type": "string" } } } } });
        let changed = json!({ "$ref": "#/$defs/Item",
                              "$defs": { "Item": { "type": "object",
                                                   "properties": { "id": { "type": "integer" } } } } });
        let changes = diff(&manifest(schema, None), &manifest(changed, None));
        assert_eq!(changes.len(), 1, "got {changes:?}");
        assert!(changes[0].detail.contains("string -> integer"));
    }

    #[test]
    fn a_removed_tool_breaks_callers_and_a_new_one_does_not() {
        let old = json!({ "tools": [ { "name": "a", "inputSchema": {} },
                                     { "name": "b", "inputSchema": {} } ] });
        let new = json!({ "tools": [ { "name": "a", "inputSchema": {} },
                                     { "name": "c", "inputSchema": {} } ] });
        let changes = diff(&old, &new);
        assert_eq!(changes.len(), 2, "got {changes:?}");
        assert_eq!(changes[0].severity, Severity::Breaking);
        assert_eq!(changes[0].at, "b");
        assert_eq!(changes[1].severity, Severity::Additive);
    }
}
