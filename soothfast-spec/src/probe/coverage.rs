//! Every field the spec declares for a response, as population paths.
//!
//! The population lock only protects fields that have been observed at
//! least once — a field declared in the schema but never wired to data is
//! invisible to it. This walk enumerates the response schema's field
//! paths in the same dotted/`[]` notation as [`super::population`], so
//! declared-but-never-populated fields can be diffed, locked, and gated.

use std::collections::BTreeSet;

use serde_json::Value;

/// Enumerate the field paths `schema` declares, resolving `$ref`s through
/// the spec's `components/schemas`. `allOf` branches all apply and are
/// merged; `oneOf`/`anyOf` children are conditional on which branch the
/// value takes, so nothing below a union counts as declared. Recursive
/// schemas stop at the repeated `$ref`; an open schema (`{}`) declares
/// nothing below its own path.
pub fn declared_paths(schema: &Value, spec: &Value) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    walk(schema, spec, String::new(), &mut Vec::new(), &mut paths);
    paths
}

fn walk(
    schema: &Value,
    spec: &Value,
    path: String,
    ref_stack: &mut Vec<String>,
    out: &mut BTreeSet<String>,
) {
    let Some(schema_obj) = schema.as_object() else {
        return;
    };
    if let Some(reference) = schema_obj.get("$ref").and_then(Value::as_str) {
        if ref_stack.iter().any(|r| r == reference) {
            return;
        }
        let Some(resolved) = super::shape::resolve(spec, reference) else {
            return;
        };
        ref_stack.push(reference.to_string());
        walk(resolved, spec, path, ref_stack, out);
        ref_stack.pop();
        return;
    }
    if schema_obj.contains_key("oneOf") || schema_obj.contains_key("anyOf") {
        return;
    }
    if let Some(branches) = schema_obj.get("allOf").and_then(Value::as_array) {
        for branch in branches {
            walk(branch, spec, path.clone(), ref_stack, out);
        }
        return;
    }
    if let Some(properties) = schema_obj.get("properties").and_then(Value::as_object) {
        for (key, child) in properties {
            let child_path = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            out.insert(child_path.clone());
            walk(child, spec, child_path, ref_stack, out);
        }
    }
    if let Some(items) = schema_obj.get("items") {
        walk(items, spec, format!("{path}[]"), ref_stack, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn properties_arrays_and_refs_enumerate_like_population_paths() {
        let spec = json!({
            "components": {"schemas": {"Money": {
                "type": "object",
                "properties": {"raw": {"type": "number"}, "fmt": {"type": "string"}},
            }}}
        });
        let schema = json!({
            "type": "object",
            "properties": {
                "symbol": {"type": "string"},
                "price": {"$ref": "#/components/schemas/Money"},
                "candles": {"type": "array", "items": {
                    "type": "object",
                    "properties": {"close": {"type": "number"}},
                }},
            },
        });
        let paths = declared_paths(&schema, &spec);
        let expected: BTreeSet<String> = [
            "symbol",
            "price",
            "price.raw",
            "price.fmt",
            "candles",
            "candles[].close",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(paths, expected);
    }

    #[test]
    fn open_and_recursive_schemas_terminate() {
        let spec = json!({
            "components": {"schemas": {"Node": {
                "type": "object",
                "properties": {
                    "value": {"type": "number"},
                    "next": {"$ref": "#/components/schemas/Node"},
                },
            }}}
        });
        let schema = json!({
            "type": "object",
            "properties": {"open": {}, "tree": {"$ref": "#/components/schemas/Node"}},
        });
        let paths = declared_paths(&schema, &spec);
        assert!(paths.contains("open"));
        assert!(paths.contains("tree.value"));
        assert!(paths.contains("tree.next"));
        assert!(!paths.contains("tree.next.value"));
    }

    #[test]
    fn union_children_are_conditional_but_all_of_is_conjunctive() {
        let spec = json!({});
        let union = json!({
            "oneOf": [
                {"type": "number"},
                {"type": "object", "properties": {"raw": {"type": "number"}}},
            ],
        });
        assert!(declared_paths(&union, &spec).is_empty());
        let conjunction = json!({
            "allOf": [
                {"type": "object", "properties": {"a": {"type": "number"}}},
                {"type": "object", "properties": {"b": {"type": "number"}}},
            ],
        });
        let paths = declared_paths(&conjunction, &spec);
        assert!(paths.contains("a"));
        assert!(paths.contains("b"));
    }
}
