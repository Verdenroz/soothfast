//! A validator for the JSON Schemas under `schemas/`, covering only the
//! keywords they use. A schema using any other keyword fails loudly instead of
//! being silently under-checked.
//!
//! It is stricter than the schemas: a key the instance carries that the schema
//! does not declare is an error even where `additionalProperties` is open, and
//! declared properties the instance never exercised are reported. That is what
//! makes a writer gaining or losing a field fail the drift tests.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

const ANNOTATIONS: [&str; 4] = ["$schema", "$id", "title", "description"];
const KEYWORDS: [&str; 8] = [
    "type",
    "required",
    "properties",
    "additionalProperties",
    "items",
    "enum",
    "const",
    "oneOf",
];

#[derive(Default, Debug)]
pub struct Report {
    pub errors: Vec<String>,
    /// Schema positions of every declared property, `*` for map values.
    declared: BTreeSet<String>,
    seen: BTreeSet<String>,
}

impl Report {
    /// Declared properties no object in the instance carried.
    pub fn unused(&self) -> Vec<&String> {
        self.declared.difference(&self.seen).collect()
    }
}

pub fn load(file: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../schemas")
        .join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Panic on any keyword the validator does not implement.
pub fn assert_supported(schema: &Value) {
    walk(schema);
}

fn walk(node: &Value) {
    let Some(map) = node.as_object() else {
        panic!("schema node is not an object: {node}");
    };
    for (key, value) in map {
        match key.as_str() {
            k if ANNOTATIONS.contains(&k) || KEYWORDS.contains(&k) => {}
            "$defs" | "$ref" => {}
            other => panic!("validator does not support `{other}`"),
        }
        match key.as_str() {
            "$defs" | "properties" => value.as_object().unwrap().values().for_each(walk),
            "additionalProperties" if value.is_object() => walk(value),
            "items" => walk(value),
            "oneOf" => value.as_array().unwrap().iter().for_each(walk),
            _ => {}
        }
    }
}

pub fn check(root: &Value, instance: &Value) -> Report {
    assert_supported(root);
    let mut report = Report::default();
    visit(root, root, instance, "$", "$", &mut report);
    report
}

fn resolve<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
    match schema.get("$ref").and_then(Value::as_str) {
        Some(r) => root
            .pointer(r.strip_prefix('#').expect("only local $ref is supported"))
            .unwrap_or_else(|| panic!("unresolved $ref {r}")),
        None => schema,
    }
}

fn type_matches(ty: &str, v: &Value) -> bool {
    match ty {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => v.is_u64() || v.is_i64(),
        other => panic!("validator does not support type `{other}`"),
    }
}

fn visit(root: &Value, schema: &Value, v: &Value, path: &str, sp: &str, out: &mut Report) {
    let schema = resolve(root, schema);
    if let Some(ty) = schema.get("type") {
        let types: Vec<&str> = match ty {
            Value::String(s) => vec![s],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => panic!("`type` must be a string or array"),
        };
        if !types.iter().any(|t| type_matches(t, v)) {
            out.errors.push(format!("{path}: expected {ty}, got {v}"));
            return;
        }
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(v)
    {
        out.errors.push(format!("{path}: {v} not in enum"));
    }
    if let Some(expected) = schema.get("const")
        && expected != v
    {
        out.errors
            .push(format!("{path}: expected {expected}, got {v}"));
    }
    if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
        one_of(root, branches, v, path, sp, out);
    } else if let Some(obj) = v.as_object() {
        visit_object(root, schema, obj, path, sp, out);
    }
    if let (Some(items), Some(arr)) = (schema.get("items"), v.as_array()) {
        for (i, e) in arr.iter().enumerate() {
            visit(
                root,
                items,
                e,
                &format!("{path}[{i}]"),
                &format!("{sp}[]"),
                out,
            );
        }
    }
}

fn one_of(root: &Value, branches: &[Value], v: &Value, path: &str, sp: &str, out: &mut Report) {
    let attempts: Vec<Report> = branches
        .iter()
        .map(|b| {
            let mut r = Report::default();
            visit(root, b, v, path, sp, &mut r);
            r
        })
        .collect();
    let mut passing = attempts.into_iter().filter(|r| r.errors.is_empty());
    match (passing.next(), passing.next()) {
        (Some(only), None) => {
            out.declared.extend(only.declared);
            out.seen.extend(only.seen);
        }
        (None, _) => out
            .errors
            .push(format!("{path}: matches no oneOf branch: {v}")),
        (Some(_), Some(_)) => out
            .errors
            .push(format!("{path}: matches several oneOf branches")),
    }
}

fn visit_object(
    root: &Value,
    schema: &Value,
    obj: &serde_json::Map<String, Value>,
    path: &str,
    sp: &str,
    out: &mut Report,
) {
    let props = schema.get("properties").and_then(Value::as_object);
    for req in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !obj.contains_key(req) {
            out.errors.push(format!("{path}: missing required `{req}`"));
        }
    }
    let extra = schema.get("additionalProperties");
    for (k, val) in obj {
        let here = format!("{path}.{k}");
        match (props.and_then(|p| p.get(k)), extra) {
            (Some(s), _) => {
                out.seen.insert(format!("{sp}.{k}"));
                visit(root, s, val, &here, &format!("{sp}.{k}"), out);
            }
            (None, Some(s)) if s.is_object() => visit(root, s, val, &here, &format!("{sp}.*"), out),
            (None, _) => out.errors.push(format!("{here}: undeclared key")),
        }
    }
    for k in props.into_iter().flat_map(|p| p.keys()) {
        out.declared.insert(format!("{sp}.{k}"));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    #[should_panic(expected = "does not support `pattern`")]
    fn an_unsupported_keyword_fails_instead_of_passing() {
        check(&json!({ "type": "string", "pattern": "^a" }), &json!("a"));
    }

    #[test]
    fn a_wrong_type_and_an_undeclared_key_are_errors() {
        let schema = json!({ "type": "object", "properties": { "n": { "type": "integer" } } });
        let report = check(&schema, &json!({ "n": 1.5, "x": 1 }));
        assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
    }

    #[test]
    fn one_of_selects_by_const_discriminator() {
        let schema = json!({ "oneOf": [
            { "type": "object", "required": ["t"], "properties": { "t": { "const": "a" } } },
            { "type": "object", "required": ["t"], "properties": { "t": { "const": "b" } } },
        ] });
        assert!(check(&schema, &json!({ "t": "b" })).errors.is_empty());
        assert!(!check(&schema, &json!({ "t": "c" })).errors.is_empty());
    }

    #[test]
    fn declared_properties_the_instance_lacks_are_reported_unused() {
        let schema = json!({ "type": "object", "properties": { "a": {}, "b": {} } });
        assert_eq!(check(&schema, &json!({ "a": 1 })).unused(), ["$.b"]);
    }
}
