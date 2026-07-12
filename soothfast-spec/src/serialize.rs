//! YAML and JSON rendering for generated documents.
//!
//! Output is byte-deterministic: schema maps are `BTreeMap`s upstream and keys
//! emit here in a fixed order. A generated file that reshuffled between runs
//! would produce an endless stream of no-op commits.

use serde_json::Value;

/// Keys emitted before any others, so a generated document reads in the order
/// a human expects rather than alphabetically.
///
/// One list covers every dialect. A key another dialect never emits simply
/// never matches, and the alternative — a table per dialect — would make two
/// documents that share a construct (`info`, `schemas`) order it differently.
const KEY_ORDER: &[&str] = &[
    // Document preamble, whichever dialect declares it.
    "openapi",
    "asyncapi",
    "info",
    // Identity before prose, wherever both appear: a tool, a parameter and a
    // server are all read by name first.
    "name",
    "title",
    "version",
    "description",
    "servers",
    "url",
    "host",
    "protocol",
    // OpenAPI operations.
    "paths",
    "operationId",
    "summary",
    "tags",
    "parameters",
    "in",
    "requestBody",
    "required",
    "content",
    "responses",
    // AsyncAPI channels and operations.
    "channels",
    "address",
    "action",
    "channel",
    "messages",
    "operations",
    "payload",
    // MCP tools.
    "tools",
    "inputSchema",
    "outputSchema",
    // JSON Schema bodies.
    "schema",
    "$ref",
    "type",
    "format",
    "properties",
    "items",
    "prefixItems",
    "oneOf",
    "allOf",
    "const",
    "enum",
    "additionalProperties",
    // Shared definitions, last because they are the document's appendix.
    "components",
    "schemas",
    "$defs",
];

/// Keys whose object *values* are keyed by data rather than by keyword: field
/// names, type names, channel addresses, media types.
///
/// Inside one of these, [`KEY_ORDER`] must not apply. A struct with a field
/// called `tags` or `type` would otherwise have it hoisted above its siblings
/// for no reason a reader could see, and whether a generated file reshuffles
/// would depend on whether someone's field name happened to collide with a
/// spec keyword.
const NAME_KEYED: &[&str] = &[
    "properties",
    "$defs",
    "schemas",
    "definitions",
    "paths",
    "responses",
    "content",
    "channels",
    "operations",
    "messages",
    "servers",
    "parameters",
    "types",
    "roots",
    "fields",
    "args",
];

/// Rank a key for emission. Ranks are doubled so an entry can be slotted
/// between two others with an odd number.
///
/// `required` is the one key whose place depends on context: a boolean on a
/// parameter, where it reads first, but an array on a schema, where
/// convention puts it after `properties`. The value type tells them apart.
fn key_rank(key: &str, value: &Value) -> usize {
    let position = |name: &str| {
        KEY_ORDER
            .iter()
            .position(|k| *k == name)
            .unwrap_or(KEY_ORDER.len())
    };
    if key == "required" && value.is_array() {
        return position("properties") * 2 + 1;
    }
    position(key) * 2
}

/// An object's keys in emission order.
///
/// `data_keyed` objects sort plainly by name; everything else leads with the
/// well-known keys.
fn ordered_keys(o: &serde_json::Map<String, Value>, data_keyed: bool) -> Vec<&String> {
    let mut keys: Vec<&String> = o.keys().collect();
    if data_keyed {
        keys.sort();
    } else {
        keys.sort_by_key(|k| (key_rank(k, &o[*k]), (*k).clone()));
    }
    keys
}

/// Render a document as YAML, with well-known keys in conventional order.
pub fn to_yaml(value: &Value) -> String {
    let mut out = String::new();
    let yaml = to_yaml_value(value, false);
    let mut emitter = yaml_rust2::YamlEmitter::new(&mut out);
    if emitter.dump(&yaml).is_err() {
        // The emitter only fails on unrepresentable values; JSON has none.
        return to_json(value);
    }
    // yaml-rust2 opens every document with a `---` marker.
    let body = out.strip_prefix("---\n").unwrap_or(&out).to_string();
    format!("{body}\n")
}

/// Render a document as pretty JSON, for `.json` spec files.
///
/// Hand-rolled rather than `serde_json::to_string_pretty` for one reason: the
/// same key order the YAML path uses. A tool manifest whose `description`
/// sorts above its `name` is valid and unreadable, and the two serializations
/// of one document should not disagree about what comes first.
pub fn to_json(value: &Value) -> String {
    let mut out = String::new();
    write_json(&mut out, value, 0, false);
    out.push('\n');
    out
}

fn write_json(out: &mut String, value: &Value, depth: usize, data_keyed: bool) {
    match value {
        Value::Object(o) if !o.is_empty() => {
            out.push_str("{\n");
            let keys = ordered_keys(o, data_keyed);
            for (i, key) in keys.iter().enumerate() {
                indent(out, depth + 1);
                out.push_str(&Value::String((*key).clone()).to_string());
                out.push_str(": ");
                write_json(out, &o[*key], depth + 1, NAME_KEYED.contains(&key.as_str()));
                if i + 1 < keys.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(out, depth);
            out.push('}');
        }
        Value::Array(a) if !a.is_empty() => {
            out.push_str("[\n");
            for (i, item) in a.iter().enumerate() {
                indent(out, depth + 1);
                write_json(out, item, depth + 1, false);
                if i + 1 < a.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(out, depth);
            out.push(']');
        }
        // Scalars and empty containers already serialize compactly and
        // correctly, escaping included.
        other => out.push_str(&other.to_string()),
    }
}

fn indent(out: &mut String, depth: usize) {
    out.extend(std::iter::repeat_n(' ', depth * 2));
}

fn to_yaml_value(value: &Value, data_keyed: bool) -> yaml_rust2::Yaml {
    use yaml_rust2::Yaml;
    match value {
        Value::Null => Yaml::Null,
        Value::Bool(b) => Yaml::Boolean(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Yaml::Integer(i),
            None => Yaml::Real(n.to_string()),
        },
        Value::String(s) => Yaml::String(s.clone()),
        Value::Array(a) => Yaml::Array(a.iter().map(|v| to_yaml_value(v, false)).collect()),
        Value::Object(o) => {
            let mut hash = yaml_rust2::yaml::Hash::new();
            for k in ordered_keys(o, data_keyed) {
                hash.insert(
                    Yaml::String(k.clone()),
                    to_yaml_value(&o[k], NAME_KEYED.contains(&k.as_str())),
                );
            }
            Yaml::Hash(hash)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_dialect_marker_leads_the_document() {
        let doc = json!({ "components": {}, "info": { "version": "1", "title": "T" },
                          "asyncapi": "3.0.0" });
        let yaml = to_yaml(&doc);
        assert!(yaml.starts_with("asyncapi: 3.0.0"), "got:\n{yaml}");
        let title = yaml.find("title:").expect("title");
        let version = yaml.find("version:").expect("version");
        assert!(title < version, "title reads before version:\n{yaml}");
    }

    #[test]
    fn rendering_is_byte_identical_between_runs() {
        let doc = json!({ "tools": [ { "inputSchema": {}, "name": "a" } ] });
        assert_eq!(to_yaml(&doc), to_yaml(&doc));
        assert_eq!(to_json(&doc), to_json(&doc));
    }

    #[test]
    fn required_reads_before_a_parameter_but_after_schema_properties() {
        let doc = json!({
            "parameters": [ { "schema": { "type": "integer" }, "required": true, "name": "id" } ],
            "components": { "schemas": { "Item": {
                "required": ["id"],
                "properties": { "id": { "type": "integer" } },
                "type": "object" } } },
        });
        let yaml = to_yaml(&doc);
        let param_required = yaml.find("required: true").expect("parameter required");
        let param_schema = yaml.find("schema:").expect("parameter schema");
        assert!(param_required < param_schema, "got:\n{yaml}");

        let props = yaml.find("properties:").expect("schema properties");
        let list = yaml.find("required:\n").expect("schema required list");
        assert!(props < list, "got:\n{yaml}");
    }

    #[test]
    fn json_output_ends_with_a_newline() {
        let out = to_json(&json!({ "openapi": "3.1.0" }));
        assert!(out.contains("\"openapi\": \"3.1.0\""), "got {out}");
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn json_reads_in_the_same_order_as_yaml() {
        let doc = json!({ "tools": [ { "description": "d", "inputSchema": {}, "name": "a" } ] });
        let out = to_json(&doc);
        let at = |needle: &str| {
            out.find(needle)
                .unwrap_or_else(|| panic!("{needle}\n{out}"))
        };
        assert!(at("\"name\"") < at("\"description\""), "got:\n{out}");
        assert!(at("\"description\"") < at("\"inputSchema\""), "got:\n{out}");
    }

    #[test]
    fn json_escapes_and_nests_exactly_as_serde_would() {
        let doc = json!({ "a": { "quote\"key": "line\nbreak \u{1f600}" },
                          "b": [1, -2.5, true, null, [], {}] });
        let out = to_json(&doc);
        let round_tripped: Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(round_tripped, doc);
    }

    #[test]
    fn a_field_named_after_a_spec_keyword_is_not_hoisted() {
        // `tags`, `type` and `name` are all real struct field names as well as
        // spec keywords; inside `properties` they are data and sort plainly.
        let doc = json!({ "components": { "schemas": { "Item": {
            "type": "object",
            "properties": { "tags": { "type": "array" },
                            "id": { "type": "string" },
                            "name": { "type": "string" } },
        } } } });
        for out in [to_yaml(&doc), to_json(&doc)] {
            let at = |needle: &str| {
                out.find(needle)
                    .unwrap_or_else(|| panic!("{needle}\n{out}"))
            };
            assert!(at("id") < at("name"), "alphabetical, got:\n{out}");
            assert!(at("name") < at("tags"), "alphabetical, got:\n{out}");
        }
    }

    #[test]
    fn keyword_ordering_still_applies_inside_each_schema_body() {
        // Only the map of field names is data; the schema under each one is
        // spec keywords again.
        let doc = json!({ "properties": { "id": { "properties": {}, "type": "object" } } });
        let yaml = to_yaml(&doc);
        assert!(
            yaml.find("type:").expect("type") < yaml.find("properties: {}").expect("props"),
            "got:\n{yaml}"
        );
    }

    #[test]
    fn empty_containers_stay_on_one_line() {
        let out = to_json(&json!({ "properties": {}, "servers": [] }));
        assert!(out.contains("\"properties\": {}"), "got:\n{out}");
        assert!(out.contains("\"servers\": []"), "got:\n{out}");
    }

    #[test]
    fn documents_carry_no_yaml_document_marker() {
        assert!(!to_yaml(&json!({ "a": 1 })).contains("---"));
    }
}
