//! Structural validation of a live response against the spec's own
//! response schema.
//!
//! `null` is accepted everywhere: the generated schemas don't model
//! nullability, and the server answers `null` for any absent optional —
//! judging nulls is the population baseline's job. Shape only asks
//! whether the data that *is* there has the declared structure, which is
//! what catches a number turning into a string or an array of objects
//! flattening into scalars.

use serde_json::Value;

/// One structural disagreement between response and schema.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    /// Dotted path into the response, `[]` for array elements.
    pub path: String,
    pub message: String,
}

/// Find the JSON response schema the spec declares for `method` on the
/// concrete `path` (path template segments `{like_this}` match anything),
/// for the given status code.
pub fn response_schema<'a>(
    spec: &'a Value,
    method: &str,
    path: &str,
    status: u16,
) -> Option<&'a Value> {
    let paths = spec.get("paths")?.as_object()?;
    let path = path.split('?').next().unwrap_or(path);
    let concrete: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let item = paths
        .iter()
        .find(|(template, _)| template_matches(template, &concrete))
        .map(|(_, item)| item)?;
    item.get(method.to_ascii_lowercase())?
        .get("responses")?
        .get(status.to_string())?
        .get("content")?
        .get("application/json")?
        .get("schema")
}

/// Whether a path template matches concrete path segments, treating any
/// `{seg}` as a wildcard. Segment counts must agree.
pub fn template_matches(template: &str, concrete: &[&str]) -> bool {
    let segments: Vec<&str> = template.split('/').filter(|s| !s.is_empty()).collect();
    segments.len() == concrete.len()
        && segments
            .iter()
            .zip(concrete)
            .all(|(seg, real)| seg.starts_with('{') || seg == real)
}

/// Validate `value` against `schema`, resolving `$ref`s through the
/// spec's `components/schemas`. Returns every violation found.
pub fn validate(value: &Value, schema: &Value, spec: &Value) -> Vec<Violation> {
    let mut violations = Vec::new();
    check(
        value,
        schema,
        spec,
        String::new(),
        &mut Vec::new(),
        &mut violations,
    );
    violations
}

fn check(
    value: &Value,
    schema: &Value,
    spec: &Value,
    path: String,
    ref_stack: &mut Vec<String>,
    out: &mut Vec<Violation>,
) {
    if value.is_null() {
        return;
    }
    let Some(schema_obj) = schema.as_object() else {
        return;
    };
    if let Some(reference) = schema_obj.get("$ref").and_then(Value::as_str) {
        if ref_stack.iter().any(|r| r == reference) {
            return;
        }
        let Some(resolved) = resolve(spec, reference) else {
            out.push(Violation {
                path,
                message: format!("unresolvable {reference}"),
            });
            return;
        };
        ref_stack.push(reference.to_string());
        check(value, resolved, spec, path, ref_stack, out);
        ref_stack.pop();
        return;
    }
    if let Some(branches) = one_of(schema_obj) {
        let matched = branches.iter().any(|branch| {
            let mut trial = Vec::new();
            check(value, branch, spec, path.clone(), ref_stack, &mut trial);
            trial.is_empty()
        });
        if !matched {
            out.push(Violation {
                path,
                message: format!("matches no schema branch, got {}", kind(value)),
            });
        }
        return;
    }
    if let Some(all) = schema_obj.get("allOf").and_then(Value::as_array) {
        for branch in all {
            check(value, branch, spec, path.clone(), ref_stack, out);
        }
        return;
    }
    if let Some(expected) = schema_obj.get("type")
        && !type_matches(value, expected)
    {
        out.push(Violation {
            path,
            message: format!("expected {}, got {}", type_names(expected), kind(value)),
        });
        return;
    }
    if let Some(allowed) = schema_obj.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        out.push(Violation {
            path,
            message: format!("{value} not in enum"),
        });
        return;
    }
    match value {
        Value::Object(fields) => {
            let properties = schema_obj.get("properties").and_then(Value::as_object);
            if let Some(properties) = properties {
                for (key, child_schema) in properties {
                    if let Some(child) = fields.get(key) {
                        let child_path = join(&path, key);
                        check(child, child_schema, spec, child_path, ref_stack, out);
                    }
                }
                if schema_obj.get("additionalProperties") == Some(&Value::Bool(false)) {
                    for key in fields.keys().filter(|k| !properties.contains_key(*k)) {
                        out.push(Violation {
                            path: join(&path, key),
                            message: "undeclared property".into(),
                        });
                    }
                }
            }
            if let Some(required) = schema_obj.get("required").and_then(Value::as_array) {
                for key in required.iter().filter_map(Value::as_str) {
                    if !fields.contains_key(key) {
                        out.push(Violation {
                            path: join(&path, key),
                            message: "required property missing".into(),
                        });
                    }
                }
            }
        }
        Value::Array(items) => {
            if let Some(item_schema) = schema_obj.get("items") {
                let element_path = format!("{path}[]");
                for item in items {
                    check(
                        item,
                        item_schema,
                        spec,
                        element_path.clone(),
                        ref_stack,
                        out,
                    );
                }
            }
        }
        _ => {}
    }
}

fn one_of(schema: &serde_json::Map<String, Value>) -> Option<&Vec<Value>> {
    schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
}

pub(super) fn resolve<'a>(spec: &'a Value, reference: &str) -> Option<&'a Value> {
    let name = reference.strip_prefix("#/components/schemas/")?;
    spec.get("components")?.get("schemas")?.get(name)
}

fn type_matches(value: &Value, expected: &Value) -> bool {
    match expected {
        Value::String(name) => single_type_matches(value, name),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .any(|name| single_type_matches(value, name)),
        _ => true,
    }
}

fn single_type_matches(value: &Value, name: &str) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn type_names(expected: &Value) -> String {
    match expected {
        Value::String(name) => name.clone(),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("|"),
        _ => "any".into(),
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> Value {
        json!({
            "paths": {
                "/v2/quote/{symbol}": {
                    "get": {
                        "responses": {
                            "200": {
                                "content": {
                                    "application/json": {
                                        "schema": {"$ref": "#/components/schemas/Quote"}
                                    }
                                }
                            }
                        }
                    }
                }
            },
            "components": {
                "schemas": {
                    "Quote": {
                        "type": "object",
                        "properties": {
                            "symbol": {"type": "string"},
                            "price": {"$ref": "#/components/schemas/Money"},
                            "tags": {"type": "array", "items": {"type": "string"}},
                        },
                        "required": ["symbol"],
                    },
                    "Money": {
                        "type": "object",
                        "properties": {"raw": {"type": "number"}, "fmt": {"type": "string"}},
                    },
                }
            }
        })
    }

    #[test]
    fn concrete_paths_match_templates() {
        let spec = spec();
        assert!(response_schema(&spec, "GET", "/v2/quote/AAPL", 200).is_some());
        assert!(response_schema(&spec, "GET", "/v2/quote/AAPL?fields=pe", 200).is_some());
        assert!(response_schema(&spec, "GET", "/v2/quote/AAPL/extra", 200).is_none());
        assert!(response_schema(&spec, "POST", "/v2/quote/AAPL", 200).is_none());
    }

    #[test]
    fn a_valid_response_through_refs_is_clean() {
        let spec = spec();
        let schema = response_schema(&spec, "GET", "/v2/quote/AAPL", 200).unwrap();
        let value = json!({"symbol": "AAPL", "price": {"raw": 1.0, "fmt": "1.00"}});
        assert_eq!(validate(&value, schema, &spec), []);
    }

    #[test]
    fn wrong_scalar_kind_is_reported_with_its_path() {
        let spec = spec();
        let schema = response_schema(&spec, "GET", "/v2/quote/AAPL", 200).unwrap();
        let value = json!({"symbol": "AAPL", "price": {"raw": "oops"}});
        let violations = validate(&value, schema, &spec);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, "price.raw");
    }

    #[test]
    fn nulls_pass_everywhere_but_missing_required_does_not() {
        let spec = spec();
        let schema = response_schema(&spec, "GET", "/v2/quote/AAPL", 200).unwrap();
        assert_eq!(
            validate(&json!({"symbol": null, "price": null}), schema, &spec),
            []
        );
        let violations = validate(&json!({"price": null}), schema, &spec);
        assert_eq!(violations[0].path, "symbol");
    }

    #[test]
    fn array_items_are_checked_per_element() {
        let spec = spec();
        let schema = response_schema(&spec, "GET", "/v2/quote/AAPL", 200).unwrap();
        let violations = validate(&json!({"symbol": "A", "tags": ["ok", 3]}), schema, &spec);
        assert_eq!(violations[0].path, "tags[]");
    }

    #[test]
    fn open_and_recursive_schemas_do_not_loop_or_flag() {
        let spec = json!({
            "components": {"schemas": {"Node": {
                "type": "object",
                "properties": {"next": {"$ref": "#/components/schemas/Node"}},
            }}}
        });
        let schema = json!({"$ref": "#/components/schemas/Node"});
        let value = json!({"next": {"next": {}}});
        assert_eq!(validate(&value, &schema, &spec), []);
        assert_eq!(validate(&json!({"anything": 1}), &json!({}), &spec), []);
    }
}
