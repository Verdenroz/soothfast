//! JSON Schema IR → the language-neutral SDK model.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use soothfast_spec::dialect::Operation;

use crate::model::{Field, Method, Model, Param, ParamLoc, Sdk, Ty};
use crate::{SdkKind, SdkOptions};

/// Lower operations into the SDK model.
///
/// Component schemas shared across operations must agree; two operations
/// producing a different schema under one name is the same identity
/// conflict the dialect emitters refuse.
///
/// `kind` decides only how wire names are *spelled* — every typing question
/// is answered the same way for every target.
pub fn lower(ops: &[Operation], opts: &SdkOptions, kind: SdkKind) -> Result<Sdk, String> {
    let components = merge_components(ops)?;
    let mut cx = Cx {
        kind,
        notes: Vec::new(),
        taken: components.keys().cloned().collect(),
        components: &components,
        variants: Vec::new(),
    };

    let mut models = Vec::new();
    let mut aliases = Vec::new();
    for (name, schema) in &components {
        match model_of(name, schema, kind, &mut cx) {
            Some(model) => models.push(model),
            None => aliases.push((name.clone(), ty_of(schema, &mut cx, name))),
        }
    }

    let mut methods: Vec<Method> = ops
        .iter()
        .map(|op| method_of(op, opts, kind, &mut cx))
        .collect::<Result<_, _>>()?;
    methods.sort_by(|a, b| a.operation_id.cmp(&b.operation_id));

    models.append(&mut cx.variants);
    models.sort_by(|a, b| a.name.cmp(&b.name));

    let reachable = reachable_names(&methods, &models, &aliases);
    models.retain(|m| reachable.contains(&m.name));
    aliases.retain(|(n, _)| reachable.contains(n));

    Ok(Sdk {
        models,
        aliases,
        methods,
        notes: dedup(cx.notes),
    })
}

/// What lowering accumulates beside the types it returns.
struct Cx<'a> {
    kind: SdkKind,
    notes: Vec<String>,
    /// Every model name in use, components and synthesized variants alike.
    taken: BTreeSet<String>,
    /// The merged component schemas, for arms that point at one.
    components: &'a BTreeMap<String, Value>,
    /// Models synthesized for inline arms of tagged unions.
    variants: Vec<Model>,
}

/// Component names reachable from any method's types. Param structs expand
/// into individual parameters, so their components are dead weight unless
/// something else references them.
fn reachable_names(
    methods: &[Method],
    models: &[Model],
    aliases: &[(String, Ty)],
) -> BTreeSet<String> {
    let mut reachable = BTreeSet::new();
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
        Ty::Tagged { arms, .. } => arms.iter().for_each(|(_, a)| collect_named(a, out)),
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
fn model_of(name: &str, schema: &Value, kind: SdkKind, cx: &mut Cx) -> Option<Model> {
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
                // A dataclass field named `self` would redeclare the one
                // its generated `__init__` already takes.
                attr: attr_for(
                    wire,
                    &at,
                    kind,
                    &mut taken,
                    &reserved_fields(kind),
                    &mut cx.notes,
                ),
                wire: wire.clone(),
                ty: ty_of(field_schema, cx, &at),
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

fn method_of(
    op: &Operation,
    opts: &SdkOptions,
    kind: SdkKind,
    cx: &mut Cx,
) -> Result<Method, String> {
    let at = &op.operation_id;
    let mut taken = BTreeMap::new();
    let reserved = reserved_args(
        kind,
        op.shape.request.is_some(),
        opts.paginated.contains(&op.operation_id),
    );
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
            // In TypeScript only path params become identifiers; the rest
            // are keys of the options object and keep their wire spelling.
            let reserved: &[&str] = match (kind, location) {
                (SdkKind::TypeScript, ParamLoc::Path) | (SdkKind::Python, _) => &reserved,
                _ => &[],
            };
            Param {
                attr: attr_for(
                    &p.name,
                    &format!("{at}({})", p.name),
                    kind,
                    &mut taken,
                    reserved,
                    &mut cx.notes,
                ),
                wire: p.name.clone(),
                location,
                ty: ty_of(&p.schema, cx, &format!("{at}({})", p.name)),
                required: p.required,
                doc: p.schema["description"].as_str().map(String::from),
            }
        })
        .collect();

    let body = op
        .shape
        .request
        .as_ref()
        .map(|b| ty_of(&b.schema, cx, &format!("{at}(body)")));

    let (ok, ok_status) = success_of(op, cx);

    Ok(Method {
        name: method_name(&op.operation_id, kind),
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
fn success_of(op: &Operation, cx: &mut Cx) -> (Ty, u16) {
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
            let ty = ty_of(&resp.schema, cx, &format!("{}[{status}]", op.operation_id));
            (ty, status)
        }
        Some((status, _)) => (Ty::Unit, status),
        None => (Ty::Unit, 200),
    }
}

/// Lower one JSON Schema node.
fn ty_of(schema: &Value, cx: &mut Cx, at: &str) -> Ty {
    if let Some(name) = ref_name(schema) {
        return Ty::Named(name.to_string());
    }
    if let Some(arms) = schema["oneOf"].as_array() {
        if let Some(tagged) = tagged_of(schema, arms, cx, at) {
            return tagged;
        }
        return Ty::Union(arms.iter().map(|a| ty_of(a, cx, at)).collect());
    }
    if let Some(members) = schema["allOf"].as_array()
        && has_only_keys(schema, |k| k == "allOf" || is_annotation_key(k))
    {
        let mut substantive = members.iter().filter(|m| !is_annotation_only(m));
        if let (Some(only), None) = (substantive.next(), substantive.next()) {
            return ty_of(only, cx, at);
        }
    }
    if let Some(value) = schema["const"].as_str() {
        return Ty::Literal(vec![value.to_string()]);
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
                    let base = ty_of(&serde_json::json!({ "type": one }), cx, at);
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
                Some(items) => Ty::List(Box::new(ty_of(items, cx, at))),
                None => {
                    if schema.get("prefixItems").is_some() {
                        cx.notes
                            .push(format!("{at}: tuple schema lowered to a plain list"));
                    }
                    Ty::List(Box::new(Ty::Any))
                }
            },
            "object" => object_ty(schema, cx, at),
            _ => Ty::Any,
        },
        _ => Ty::Any,
    }
}

/// The component name a `$ref` points at.
fn ref_name(schema: &Value) -> Option<&str> {
    let r = schema["$ref"].as_str()?;
    Some(r.rsplit('/').next().unwrap_or(r))
}

fn is_annotation_key(key: &str) -> bool {
    matches!(key, "description" | "title" | "default" | "examples")
}

fn has_only_keys(schema: &Value, allowed: impl Fn(&str) -> bool) -> bool {
    schema
        .as_object()
        .is_some_and(|o| o.keys().all(|k| allowed(k)))
}

fn is_annotation_only(schema: &Value) -> bool {
    has_only_keys(schema, is_annotation_key)
}

/// A `oneOf` whose every arm names itself with a distinct string `const` on
/// one shared property: an inline object carrying the tag, an `allOf` of a
/// tag-only object and a `$ref` payload, or a `$ref` to a component that
/// carries the tag. An OpenAPI `discriminator` picks the property when it
/// names one every arm has.
fn tagged_of(schema: &Value, arms: &[Value], cx: &mut Cx, at: &str) -> Option<Ty> {
    let parsed: Vec<Arm> = arms
        .iter()
        .map(|a| arm_of(a, cx.components))
        .collect::<Option<_>>()?;
    let hint = schema["discriminator"]["propertyName"].as_str();
    let tag = hint
        .into_iter()
        .chain(parsed.first()?.consts.keys().map(String::as_str))
        .find(|tag| {
            let mut seen = BTreeSet::new();
            parsed
                .iter()
                .all(|a| a.consts.get(*tag).is_some_and(|v| seen.insert(v)))
        })?
        .to_string();

    let lowered = parsed
        .iter()
        .map(|arm| {
            let value = &arm.consts[&tag];
            let ty = match arm.payload {
                Some(payload) => ty_of(payload, cx, at),
                None => Ty::Named(variant_model(arm.schema, value, cx, at)?),
            };
            Some((value.clone(), ty))
        })
        .collect::<Option<_>>()?;
    Some(Ty::Tagged { tag, arms: lowered })
}

struct Arm<'a> {
    schema: &'a Value,
    /// String-const properties of the arm's tag-bearing object.
    consts: BTreeMap<String, String>,
    /// The `$ref` the arm lowers through; `None` for an inline object.
    payload: Option<&'a Value>,
}

fn arm_of<'a>(schema: &'a Value, components: &BTreeMap<String, Value>) -> Option<Arm<'a>> {
    if let Some(name) = ref_name(schema) {
        return Some(Arm {
            schema,
            consts: consts_of(components.get(name)?)?,
            payload: Some(schema),
        });
    }
    if let Some([a, b]) = schema["allOf"].as_array().map(Vec::as_slice) {
        let (tag_only, payload) = if b.get("$ref").is_some() {
            (a, b)
        } else {
            (b, a)
        };
        let consts = consts_of(tag_only)?;
        let all_const = tag_only["properties"]
            .as_object()
            .is_some_and(|p| p.len() == consts.len());
        return (payload.get("$ref").is_some() && all_const).then_some(Arm {
            schema,
            consts,
            payload: Some(payload),
        });
    }
    Some(Arm {
        schema,
        consts: consts_of(schema)?,
        payload: None,
    })
}

fn consts_of(schema: &Value) -> Option<BTreeMap<String, String>> {
    let consts: BTreeMap<String, String> = schema["properties"]
        .as_object()?
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), v["const"].as_str()?.to_string())))
        .collect();
    (!consts.is_empty()).then_some(consts)
}

/// Synthesize the model for an inline arm, named after where the union sits
/// and the arm's tag value.
fn variant_model(schema: &Value, value: &str, cx: &mut Cx, at: &str) -> Option<String> {
    let base = format!(
        "{}{}",
        crate::naming::pascal(at),
        crate::naming::pascal(value)
    );
    let mut name = base.clone();
    let mut n = 1;
    while !cx.taken.insert(name.clone()) {
        n += 1;
        name = format!("{base}{n}");
    }
    let model = model_of(&name, schema, cx.kind, cx)?;
    cx.variants.push(model);
    Some(name)
}

fn object_ty(schema: &Value, cx: &mut Cx, at: &str) -> Ty {
    if schema.get("properties").is_some() {
        cx.notes.push(format!(
            "{at}: anonymous object schema lowered to an open map"
        ));
        return Ty::Map(Box::new(Ty::Any));
    }
    match schema.get("additionalProperties") {
        Some(Value::Object(_)) => Ty::Map(Box::new(ty_of(&schema["additionalProperties"], cx, at))),
        _ => Ty::Map(Box::new(Ty::Any)),
    }
}

/// How the target spells an operation id as a method name.
fn method_name(operation_id: &str, kind: SdkKind) -> String {
    match kind {
        SdkKind::Python => crate::python::naming::snake(operation_id),
        SdkKind::TypeScript => crate::typescript::naming::camel(operation_id),
    }
}

/// A collision-free target-language attribute for a wire name.
///
/// TypeScript stays wire-faithful — property keys are quoted where they
/// have to be — so the mapping is the identity and cannot collide. Python
/// snake_cases, which can, and then falls back to the wire spelling.
/// Names a generated model already uses for itself.
fn reserved_fields(kind: SdkKind) -> Vec<&'static str> {
    match kind {
        SdkKind::TypeScript => Vec::new(),
        SdkKind::Python => vec!["self"],
    }
}

/// Names the emitted code gives its own arguments. A wire name landing on
/// one of these would redeclare it, so it is moved aside like any other
/// collision — a duplicate argument is a file that will not even parse.
fn reserved_args(kind: SdkKind, has_body: bool, paginated: bool) -> Vec<&'static str> {
    let mut out = match kind {
        // Only path params are positional in TypeScript; query params live
        // inside the options object, where wire spelling must survive.
        SdkKind::TypeScript => vec!["options"],
        SdkKind::Python => vec!["self"],
    };
    if has_body {
        out.push("body");
    }
    if paginated && kind == SdkKind::Python {
        out.push("limit");
    }
    out
}

fn attr_for(
    wire: &str,
    at: &str,
    kind: SdkKind,
    taken: &mut BTreeMap<String, String>,
    reserved: &[&str],
    notes: &mut Vec<String>,
) -> String {
    // TypeScript keeps wire spellings; Python snake_cases them.
    let mut attr = match kind {
        SdkKind::TypeScript => wire.to_string(),
        SdkKind::Python => crate::python::naming::snake_attr(wire),
    };

    let clash = if reserved.contains(&attr.as_str()) {
        Some("the argument the client gives itself".to_string())
    } else {
        taken
            .get(&attr)
            .filter(|other| *other != wire)
            .map(|other| format!("`{other}` after snake_case"))
    };

    if let Some(other) = clash {
        attr = match kind {
            SdkKind::TypeScript => format!("{attr}_"),
            SdkKind::Python => crate::python::naming::sanitize_ident(wire),
        };
        while taken.contains_key(&attr) || reserved.contains(&attr.as_str()) {
            attr.push('_');
        }
        notes.push(format!(
            "{at}: `{wire}` collides with {other}; emitted as `{attr}`"
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

#[cfg(test)]
mod tests {
    use serde_json::json;
    use soothfast_spec::schema::RouteShape;
    use soothfast_spec::schema::route_sig::Response;

    use super::*;

    fn lowered(components: &[(&str, Value)], response: Value) -> Sdk {
        let mut shape = RouteShape::default();
        for (name, schema) in components {
            shape.components.insert((*name).into(), schema.clone());
        }
        shape
            .responses
            .insert("200".into(), Response::json(response));
        let op = Operation {
            operation_id: "getEvent".into(),
            method: "GET".into(),
            path: "/event".into(),
            summary: None,
            shape,
        };
        lower(&[op], &SdkOptions::default(), SdkKind::TypeScript).expect("lowers")
    }

    fn payload(name: &str) -> (&str, Value) {
        (
            name,
            json!({ "type": "object", "properties": { "id": { "type": "integer" } } }),
        )
    }

    fn graphql_arm(typename: &str) -> Value {
        json!({ "allOf": [
            { "type": "object", "properties": { "__typename": { "const": typename } } },
            { "$ref": format!("#/components/schemas/{typename}") },
        ] })
    }

    #[test]
    fn allof_arms_become_a_tagged_union_that_reaches_the_payloads() {
        let sdk = lowered(
            &[payload("Earnings"), payload("Split"), payload("Unused")],
            json!({ "oneOf": [graphql_arm("Earnings"), graphql_arm("Split")] }),
        );
        assert_eq!(
            sdk.methods[0].ok,
            Ty::Tagged {
                tag: "__typename".into(),
                arms: vec![
                    ("Earnings".into(), Ty::Named("Earnings".into())),
                    ("Split".into(), Ty::Named("Split".into())),
                ],
            }
        );
        let names: Vec<&str> = sdk.models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["Earnings", "Split"]);
    }

    #[test]
    fn inline_arms_get_a_model_each_named_by_where_they_sit() {
        let arm = |kind: &str, field: &str| {
            json!({
                "type": "object",
                "properties": { "kind": { "const": kind }, field: { "type": "string" } },
                "required": ["kind"],
            })
        };
        let sdk = lowered(
            &[],
            json!({ "oneOf": [arm("circle", "r"), arm("square", "side")] }),
        );
        assert_eq!(
            sdk.methods[0].ok,
            Ty::Tagged {
                tag: "kind".into(),
                arms: vec![
                    ("circle".into(), Ty::Named("GetEvent200Circle".into())),
                    ("square".into(), Ty::Named("GetEvent200Square".into())),
                ],
            }
        );
        let circle = &sdk.models[0];
        assert_eq!(circle.name, "GetEvent200Circle");
        assert_eq!(circle.fields[0].ty, Ty::Literal(vec!["circle".into()]));
        assert!(circle.fields[0].required);
    }

    #[test]
    fn a_repeated_tag_value_keeps_the_structural_union() {
        let sdk = lowered(
            &[payload("Earnings")],
            json!({ "oneOf": [graphql_arm("Earnings"), graphql_arm("Earnings")] }),
        );
        assert!(matches!(sdk.methods[0].ok, Ty::Union(_)));
    }

    #[test]
    fn an_arm_without_the_shared_tag_keeps_the_structural_union() {
        let sdk = lowered(
            &[payload("Earnings")],
            json!({ "oneOf": [graphql_arm("Earnings"), { "type": "string" }] }),
        );
        assert!(matches!(sdk.methods[0].ok, Ty::Union(_)));
    }

    #[test]
    fn an_allof_with_its_own_properties_is_not_collapsed_to_the_ref() {
        let sdk = lowered(
            &[payload("Earnings")],
            json!({
                "allOf": [{ "$ref": "#/components/schemas/Earnings" }],
                "type": "object",
                "properties": { "extra": { "type": "string" } },
            }),
        );
        assert_ne!(sdk.methods[0].ok, Ty::Named("Earnings".into()));
    }

    #[test]
    fn an_allof_of_one_ref_lowers_to_the_ref() {
        let sdk = lowered(
            &[payload("Earnings")],
            json!({ "allOf": [{ "$ref": "#/components/schemas/Earnings" }], "description": "d" }),
        );
        assert_eq!(sdk.methods[0].ok, Ty::Named("Earnings".into()));
    }

    #[test]
    fn ref_arms_whose_components_carry_the_tag_are_tagged() {
        let shape = |kind: &str| {
            json!({
                "type": "object",
                "properties": { "kind": { "const": kind }, "id": { "type": "integer" } },
            })
        };
        let sdk = lowered(
            &[("Circle", shape("circle")), ("Square", shape("square"))],
            json!({ "oneOf": [
                { "$ref": "#/components/schemas/Circle" },
                { "$ref": "#/components/schemas/Square" },
            ] }),
        );
        assert_eq!(
            sdk.methods[0].ok,
            Ty::Tagged {
                tag: "kind".into(),
                arms: vec![
                    ("circle".into(), Ty::Named("Circle".into())),
                    ("square".into(), Ty::Named("Square".into())),
                ],
            }
        );
    }

    #[test]
    fn a_discriminator_picks_the_tag_among_several_const_properties() {
        let shape = |kind: &str, version: &str| {
            json!({
                "type": "object",
                "properties": { "kind": { "const": kind }, "v": { "const": version } },
            })
        };
        let sdk = lowered(
            &[("A", shape("a", "1")), ("B", shape("b", "2"))],
            json!({
                "oneOf": [
                    { "$ref": "#/components/schemas/A" },
                    { "$ref": "#/components/schemas/B" },
                ],
                "discriminator": { "propertyName": "v" },
            }),
        );
        let Ty::Tagged { tag, arms } = &sdk.methods[0].ok else {
            panic!("expected a tagged union");
        };
        assert_eq!(tag, "v");
        assert_eq!(arms[0].0, "1");
    }

    #[test]
    fn colliding_variant_names_take_a_numeric_suffix() {
        let arm =
            |kind: &str| json!({ "type": "object", "properties": { "kind": { "const": kind } } });
        let sdk = lowered(
            &[(
                "GetEvent200Circle",
                json!({ "type": "object", "properties": {} }),
            )],
            json!({ "oneOf": [arm("circle"), arm("square")] }),
        );
        let Ty::Tagged { arms, .. } = &sdk.methods[0].ok else {
            panic!("expected a tagged union");
        };
        assert_eq!(arms[0].1, Ty::Named("GetEvent200Circle2".into()));
    }
}
