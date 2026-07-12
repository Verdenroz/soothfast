//! JSON Schema IR → the language-neutral SDK model.

use std::collections::BTreeMap;

use serde_json::Value;
use soothfast_spec::dialect::Operation;

use crate::SdkOptions;
use crate::model::{Field, Method, Model, Param, ParamLoc, Sdk, Ty};
use crate::python::naming;

/// Lower operations into the SDK model.
///
/// Component schemas shared across operations must agree; two operations
/// producing a different schema under one name is the same identity
/// conflict the dialect emitters refuse.
pub fn lower(ops: &[Operation], opts: &SdkOptions) -> Result<Sdk, String> {
    let mut notes = Vec::new();
    let components = merge_components(ops)?;

    let mut models = Vec::new();
    let mut aliases = Vec::new();
    for (name, schema) in &components {
        match model_of(name, schema, &mut notes) {
            Some(model) => models.push(model),
            None => aliases.push((name.clone(), ty_of(schema, &mut notes, name))),
        }
    }

    let mut methods: Vec<Method> = ops
        .iter()
        .map(|op| method_of(op, opts, &mut notes))
        .collect::<Result<_, _>>()?;
    methods.sort_by(|a, b| a.operation_id.cmp(&b.operation_id));

    let reachable = reachable_names(&methods, &models, &aliases);
    models.retain(|m| reachable.contains(&m.name));
    aliases.retain(|(n, _)| reachable.contains(n));

    Ok(Sdk {
        models,
        aliases,
        methods,
        notes: dedup(notes),
    })
}

/// Component names reachable from any method's types. Param structs expand
/// into individual parameters, so their components are dead weight unless
/// something else references them.
fn reachable_names(
    methods: &[Method],
    models: &[Model],
    aliases: &[(String, Ty)],
) -> std::collections::BTreeSet<String> {
    let mut reachable = std::collections::BTreeSet::new();
    let mut queue: Vec<String> = Vec::new();
    for m in methods {
        for p in &m.params {
            collect_named(&p.ty, &mut queue);
        }
        if let Some(body) = &m.body {
            collect_named(body, &mut queue);
        }
        collect_named(&m.ok, &mut queue);
    }
    while let Some(name) = queue.pop() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        if let Some(model) = models.iter().find(|m| m.name == name) {
            for f in &model.fields {
                collect_named(&f.ty, &mut queue);
            }
        }
        if let Some((_, ty)) = aliases.iter().find(|(n, _)| *n == name) {
            collect_named(ty, &mut queue);
        }
    }
    reachable
}

fn collect_named(ty: &Ty, out: &mut Vec<String>) {
    match ty {
        Ty::Named(n) => out.push(n.clone()),
        Ty::List(t) | Ty::Map(t) | Ty::Optional(t) => collect_named(t, out),
        Ty::Union(arms) => arms.iter().for_each(|a| collect_named(a, out)),
        _ => {}
    }
}

fn merge_components(ops: &[Operation]) -> Result<BTreeMap<String, Value>, String> {
    let mut components: BTreeMap<String, Value> = BTreeMap::new();
    for op in ops {
        for (name, schema) in &op.shape.components {
            match components.get(name) {
                Some(existing) if existing != schema => {
                    return Err(format!(
                        "component `{name}` has two different schemas \
                         (reached via operation `{}`)",
                        op.operation_id
                    ));
                }
                Some(_) => {}
                None => {
                    components.insert(name.clone(), schema.clone());
                }
            }
        }
    }
    Ok(components)
}

/// An object schema with named properties becomes a model; anything else
/// stays an alias.
fn model_of(name: &str, schema: &Value, notes: &mut Vec<String>) -> Option<Model> {
    let props = schema.get("properties")?.as_object()?;
    let required: Vec<&str> = schema["required"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut taken = BTreeMap::new();
    let fields = props
        .iter()
        .map(|(wire, field_schema)| {
            let at = format!("{name}.{wire}");
            Field {
                attr: attr_for(wire, &at, &mut taken, notes),
                wire: wire.clone(),
                ty: ty_of(field_schema, notes, &at),
                required: required.contains(&wire.as_str()),
                doc: field_schema["description"].as_str().map(String::from),
            }
        })
        .collect();

    Some(Model {
        name: name.to_string(),
        doc: schema["description"].as_str().map(String::from),
        fields,
    })
}

fn method_of(op: &Operation, opts: &SdkOptions, notes: &mut Vec<String>) -> Result<Method, String> {
    let at = &op.operation_id;
    let mut taken = BTreeMap::new();
    let params = op
        .shape
        .parameters
        .iter()
        .map(|p| {
            let location = match p.location.as_str() {
                "path" => ParamLoc::Path,
                "header" => ParamLoc::Header,
                _ => ParamLoc::Query,
            };
            Param {
                attr: attr_for(&p.name, &format!("{at}({})", p.name), &mut taken, notes),
                wire: p.name.clone(),
                location,
                ty: ty_of(&p.schema, notes, &format!("{at}({})", p.name)),
                required: p.required,
                doc: p.schema["description"].as_str().map(String::from),
            }
        })
        .collect();

    let body = op
        .shape
        .request
        .as_ref()
        .map(|b| ty_of(&b.schema, notes, &format!("{at}(body)")));

    let (ok, ok_status) = success_of(op, notes);

    Ok(Method {
        name: naming::snake(&op.operation_id),
        operation_id: op.operation_id.clone(),
        http_method: op.method.clone(),
        path: op.path.clone(),
        doc: op.summary.clone(),
        params,
        body,
        ok,
        ok_status,
        paginated: opts.paginated.contains(&op.operation_id),
    })
}

/// The success response: the lowest 2xx status, or no content at all.
fn success_of(op: &Operation, notes: &mut Vec<String>) -> (Ty, u16) {
    let success = op
        .shape
        .responses
        .iter()
        .filter_map(|(code, resp)| {
            code.parse::<u16>()
                .ok()
                .filter(|s| (200..300).contains(s))
                .map(|s| (s, resp))
        })
        .min_by_key(|(s, _)| *s);
    match success {
        Some((status, resp)) if !resp.content_type.is_empty() && !resp.schema.is_null() => {
            let ty = ty_of(
                &resp.schema,
                notes,
                &format!("{}[{status}]", op.operation_id),
            );
            (ty, status)
        }
        Some((status, _)) => (Ty::Unit, status),
        None => (Ty::Unit, 200),
    }
}

/// Lower one JSON Schema node.
fn ty_of(schema: &Value, notes: &mut Vec<String>, at: &str) -> Ty {
    if let Some(r) = schema["$ref"].as_str() {
        let name = r.rsplit('/').next().unwrap_or(r);
        return Ty::Named(name.to_string());
    }
    if let Some(arms) = schema["oneOf"].as_array() {
        return Ty::Union(arms.iter().map(|a| ty_of(a, notes, at)).collect());
    }
    if let Some(values) = schema["enum"].as_array() {
        let strings: Vec<String> = values
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        if strings.len() == values.len() && !strings.is_empty() {
            return Ty::Literal(strings);
        }
    }

    match &schema["type"] {
        Value::Array(types) => {
            let non_null: Vec<&str> = types
                .iter()
                .filter_map(|t| t.as_str())
                .filter(|t| *t != "null")
                .collect();
            match non_null.as_slice() {
                [one] => {
                    let base = ty_of(&serde_json::json!({ "type": one }), notes, at);
                    Ty::Optional(Box::new(base))
                }
                _ => Ty::Any,
            }
        }
        Value::String(t) => match t.as_str() {
            "string" => Ty::Str,
            "integer" => Ty::Int,
            "number" => Ty::Float,
            "boolean" => Ty::Bool,
            "null" => Ty::Unit,
            "array" => match schema.get("items") {
                Some(items) => Ty::List(Box::new(ty_of(items, notes, at))),
                None => {
                    if schema.get("prefixItems").is_some() {
                        notes.push(format!("{at}: tuple schema lowered to a plain list"));
                    }
                    Ty::List(Box::new(Ty::Any))
                }
            },
            "object" => object_ty(schema, notes, at),
            _ => Ty::Any,
        },
        _ => Ty::Any,
    }
}

fn object_ty(schema: &Value, notes: &mut Vec<String>, at: &str) -> Ty {
    if schema.get("properties").is_some() {
        notes.push(format!(
            "{at}: anonymous object schema lowered to an open map"
        ));
        return Ty::Map(Box::new(Ty::Any));
    }
    match schema.get("additionalProperties") {
        Some(Value::Object(_)) => {
            Ty::Map(Box::new(ty_of(&schema["additionalProperties"], notes, at)))
        }
        _ => Ty::Map(Box::new(Ty::Any)),
    }
}

/// A collision-free target-language attribute for a wire name.
fn attr_for(
    wire: &str,
    at: &str,
    taken: &mut BTreeMap<String, String>,
    notes: &mut Vec<String>,
) -> String {
    let mut attr = naming::snake_attr(wire);
    if taken.get(&attr).is_some_and(|other| other != wire) {
        let other = taken[&attr].clone();
        attr = naming::sanitize_ident(wire);
        while taken.contains_key(&attr) {
            attr.push('_');
        }
        notes.push(format!(
            "{at}: `{wire}` collides with `{other}` after snake_case; \
             emitted as `{attr}`"
        ));
    }
    taken.insert(attr.clone(), wire.to_string());
    attr
}

fn dedup(mut notes: Vec<String>) -> Vec<String> {
    notes.sort();
    notes.dedup();
    notes
}
