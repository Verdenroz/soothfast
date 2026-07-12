//! Handler signatures → the wire contract they implement.
//!
//! A handler's parameters and return type already state what it accepts and
//! returns; restating that in attributes is the duplication this avoids. The
//! framework's extractor wrappers (`Json<T>`, `Query<T>`, ...) are what mark
//! which parameters are part of the contract and which are ambient context,
//! so they are matched through an extensible table rather than a fixed list —
//! an unlisted framework should cost a config line, not a code change.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::types::{Resolver, Subst};
use super::{Gap, TypeTable};

/// What a handler parameter contributes to the wire contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// A request body of the given content type.
    Body(String),
    /// Query-string parameters, expanded from the wrapped type's fields.
    Query,
    /// Path parameters, matched against the route template's placeholders.
    Path,
    /// Header parameters.
    Header,
    /// Ambient context (state, connection info) — not part of the contract.
    Ignored,
}

/// Maps extractor wrapper types to the role they play.
#[derive(Debug, Clone)]
pub struct Extractors {
    map: BTreeMap<String, Role>,
}

impl Default for Extractors {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Extractors {
    /// Wrappers used by the common Rust web frameworks, matched on the final
    /// path segment so `web::Json` and `Json` resolve alike.
    pub fn builtin() -> Self {
        let json = Role::Body("application/json".into());
        let form = Role::Body("application/x-www-form-urlencoded".into());
        let mut map = BTreeMap::new();
        for (k, v) in [
            ("Json", json.clone()),
            ("Form", form),
            ("Bytes", Role::Body("application/octet-stream".into())),
            ("Query", Role::Query),
            ("Path", Role::Path),
            ("TypedHeader", Role::Header),
            // Ambient context: present in the signature, absent from the wire.
            ("State", Role::Ignored),
            ("Extension", Role::Ignored),
            ("Data", Role::Ignored),
            ("HeaderMap", Role::Ignored),
            ("Request", Role::Ignored),
            ("ConnectInfo", Role::Ignored),
            ("Method", Role::Ignored),
            ("Uri", Role::Ignored),
        ] {
            map.insert(k.to_string(), v);
        }
        Self { map }
    }

    /// Register or override a wrapper, for frameworks outside the built-ins.
    pub fn insert(&mut self, wrapper: impl Into<String>, role: Role) {
        self.map.insert(wrapper.into(), role);
    }

    /// The role of a wrapper type, by its final path segment.
    pub fn role(&self, path: &str) -> Option<&Role> {
        self.map.get(path.rsplit("::").next().unwrap_or(path))
    }
}

/// One query, path or header parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Parameter {
    pub name: String,
    /// OpenAPI `in`: `query`, `path` or `header`.
    pub location: String,
    pub required: bool,
    pub schema: Value,
}

/// A request body and the content type it arrives as.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestBody {
    pub content_type: String,
    pub schema: Value,
    pub required: bool,
}

/// One response, keyed by status code in [`RouteShape`].
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    /// Empty for a no-content response.
    pub content_type: String,
    pub schema: Value,
    /// Response header names, rendered by dialects that can express them.
    pub headers: Vec<String>,
    /// Human description; dialects fall back to the status code's phrase.
    pub description: Option<String>,
}

impl Response {
    /// A JSON-bodied response with no headers or custom description.
    pub fn json(schema: Value) -> Self {
        Response {
            content_type: "application/json".into(),
            schema,
            headers: Vec::new(),
            description: None,
        }
    }

    /// A response with no body at all.
    pub fn empty() -> Self {
        Response {
            content_type: String::new(),
            schema: Value::Null,
            headers: Vec::new(),
            description: None,
        }
    }
}

/// Everything a handler signature says about its wire contract.
#[derive(Debug, Clone, Default)]
pub struct RouteShape {
    /// First paragraph of the handler's doc comment, if it has one.
    pub summary: Option<String>,
    pub request: Option<RequestBody>,
    pub parameters: Vec<Parameter>,
    /// Status code (or `default`) → response.
    pub responses: BTreeMap<String, Response>,
    pub components: BTreeMap<String, Value>,
    pub gaps: Vec<Gap>,
}

/// Attribute overrides for what inference cannot see.
///
/// `request` and `response` take a type name, or `[Type]` for a bare
/// array of it.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    /// Type name for the request body.
    pub request: Option<String>,
    /// Type name for the success response — the escape hatch for erased
    /// returns, which carry only a trait bound.
    pub response: Option<String>,
    /// Success status code; defaults to 200, or 204 for a unit return.
    pub status: Option<u16>,
    /// Type whose fields flatten into query parameters — for marker fns
    /// whose empty signature carries no `Query<T>` extractor to infer from.
    pub params: Option<String>,
}

impl Overrides {
    /// Whether these overrides alone are enough to build a
    /// [`detached_shape`] — a detached marker's whole contract is its
    /// attributes, so any one of them (not just `response`) is sufficient.
    fn is_detached_contract(&self) -> bool {
        self.request.is_some() || self.response.is_some() || self.params.is_some()
    }
}

/// Infer the wire contract of one handler.
///
/// `handler_path` is the `#[route]` id (`app::routes::create_item`) and
/// `route_path` the URL template, whose `{placeholders}` name tuple path
/// parameters that the Rust signature leaves positional.
pub fn infer(
    doc: &Value,
    table: &TypeTable,
    extractors: &Extractors,
    handler_path: &str,
    route_path: &str,
    overrides: &Overrides,
) -> Result<RouteShape, String> {
    let mut r = Resolver::new(doc, table)?;
    let mut shape = match r.find_by_path(handler_path).cloned() {
        Some(item) => inferred_shape(
            &mut r,
            extractors,
            handler_path,
            route_path,
            overrides,
            &item,
        )?,
        None if overrides.is_detached_contract() => {
            detached_shape(&mut r, overrides, handler_path)?
        }
        None => {
            return Err(format!(
                "handler `{handler_path}` is not in this crate's rustdoc JSON — \
                 private items need `--document-private-items`, and a marker fn \
                 outside the lib target needs `request = \"...\"`, `response = \"...\"`, \
                 or `params = \"...\"`"
            ));
        }
    };

    synthesize_path_params(&mut shape, route_path);

    shape.components = r.components;
    shape.gaps = r.gaps;
    Ok(shape)
}

/// Infer a shape from the handler's own rustdoc item, then apply overrides.
fn inferred_shape(
    r: &mut Resolver,
    extractors: &Extractors,
    handler_path: &str,
    route_path: &str,
    overrides: &Overrides,
    item: &Value,
) -> Result<RouteShape, String> {
    let sig = item["inner"]["function"]["sig"].clone();
    if sig.is_null() {
        return Err(format!("`{handler_path}` is not a function"));
    }

    let subst = Subst::new();
    let mut shape = RouteShape {
        summary: item["docs"]
            .as_str()
            .filter(|d| !d.is_empty())
            .map(|d| d.split("\n\n").next().unwrap_or(d).trim().to_string()),
        ..RouteShape::default()
    };
    let ctx = Ctx {
        extractors,
        route_path,
        at: handler_path,
        subst: &subst,
    };

    for input in sig["inputs"].as_array().cloned().unwrap_or_default() {
        let name = input[0].as_str().unwrap_or("").to_string();
        classify_input(r, &ctx, &mut shape, &name, &input[1]);
    }

    resolve_output(r, &ctx, &mut shape, &sig["output"]);

    // Overrides win: they exist for shapes inference provably cannot see.
    apply_overrides(r, &mut shape, overrides, handler_path);
    Ok(shape)
}

/// Build a shape purely from attribute overrides.
///
/// For a detached marker the attributes are the whole contract, so a type
/// name that cannot be resolved is an error here — the silent tolerance
/// [`apply_overrides`] extends to inferred shapes would leave an empty
/// operation behind a typo.
fn detached_shape(r: &mut Resolver, o: &Overrides, at: &str) -> Result<RouteShape, String> {
    let named = |r: &mut Resolver, name: &str, role: &str| {
        resolve_named_in(r, name, at).ok_or_else(|| {
            format!(
                "{role} type `{name}` for detached route `{at}` is not in \
                 this crate's rustdoc JSON"
            )
        })
    };
    let mut shape = RouteShape::default();

    if let Some(name) = &o.request {
        let schema = named(r, name, "request")?;
        shape.request = Some(RequestBody {
            content_type: "application/json".into(),
            schema,
            required: true,
        });
    }
    if let Some(name) = &o.params {
        let schema = named(r, name, "params")?;
        if !expand_query_override(r, &mut shape, &schema) {
            return Err(format!(
                "params type `{name}` for detached route `{at}` must be a \
                 struct with named fields"
            ));
        }
    }
    if let Some(name) = &o.response {
        let schema = named(r, name, "response")?;
        let status = o
            .status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "200".into());
        shape.responses.insert(status, Response::json(schema));
    }
    Ok(shape)
}

/// Replace the shape's query parameters with a struct schema's fields.
/// Returns false when the schema has no named fields to expand.
fn expand_query_override(r: &Resolver, shape: &mut RouteShape, schema: &Value) -> bool {
    let object = deref(schema, &r.components);
    let Some(props) = object.get("properties").and_then(|p| p.as_object()) else {
        return false;
    };
    let required: Vec<&str> = object["required"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    shape.parameters.retain(|p| p.location != "query");
    for (name, schema) in props {
        shape.parameters.push(Parameter {
            name: name.clone(),
            location: "query".into(),
            required: required.contains(&name.as_str()),
            schema: schema.clone(),
        });
    }
    true
}

/// Append a required string path parameter for every `{placeholder}` in the
/// route template that nothing else declared.
fn synthesize_path_params(shape: &mut RouteShape, route_path: &str) {
    for name in path_placeholders(route_path) {
        let declared = shape
            .parameters
            .iter()
            .any(|p| p.location == "path" && p.name == name);
        if !declared {
            shape.parameters.push(Parameter {
                name,
                location: "path".into(),
                required: true,
                schema: json!({"type": "string"}),
            });
        }
    }
}

/// Ambient inputs shared across one handler's inference.
struct Ctx<'c> {
    extractors: &'c Extractors,
    route_path: &'c str,
    /// The handler path, used as the root of gap locations.
    at: &'c str,
    subst: &'c Subst,
}

fn classify_input(
    r: &mut Resolver,
    ctx: &Ctx,
    shape: &mut RouteShape,
    param_name: &str,
    ty: &Value,
) {
    let Some(wrapper) = ty["resolved_path"]["path"].as_str() else {
        return; // Bare types in parameter position are framework context.
    };
    let Some(role) = ctx.extractors.role(wrapper).cloned() else {
        return;
    };
    let inner = first_arg(&ty["resolved_path"]);
    let where_ = format!("{}({param_name})", ctx.at);

    match role {
        Role::Body(content_type) => {
            let schema = match &inner {
                Some(t) => r.resolve(t, ctx.subst, &where_),
                None => json!({}),
            };
            shape.request = Some(RequestBody {
                content_type,
                schema,
                required: true,
            });
        }
        Role::Query => expand_params(r, ctx, shape, inner.as_ref(), "query", &where_),
        Role::Path => expand_params(r, ctx, shape, inner.as_ref(), "path", &where_),
        Role::Header => expand_params(r, ctx, shape, inner.as_ref(), "header", &where_),
        Role::Ignored => {}
    }
}

/// Expand an extractor's wrapped type into individual parameters.
///
/// A struct contributes one parameter per field; a tuple is positional and
/// takes its names from the route template; a scalar takes the template's
/// sole placeholder.
fn expand_params(
    r: &mut Resolver,
    ctx: &Ctx,
    shape: &mut RouteShape,
    inner: Option<&Value>,
    location: &str,
    at: &str,
) {
    let Some(inner) = inner else { return };
    let placeholders = path_placeholders(ctx.route_path);

    if let Some(elems) = inner.get("tuple").and_then(|t| t.as_array()) {
        for (i, elem) in elems.iter().enumerate() {
            let schema = r.resolve(elem, ctx.subst, at);
            shape.parameters.push(Parameter {
                name: placeholders
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("p{i}")),
                location: location.to_string(),
                required: true,
                schema,
            });
        }
        return;
    }

    let resolved = r.resolve(inner, ctx.subst, at);
    let object = deref(&resolved, &r.components);
    if let Some(props) = object.get("properties").and_then(|p| p.as_object()) {
        let required: Vec<&str> = object["required"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        for (name, schema) in props {
            shape.parameters.push(Parameter {
                name: name.clone(),
                location: location.to_string(),
                required: required.contains(&name.as_str()),
                schema: schema.clone(),
            });
        }
        return;
    }

    // A scalar extractor names itself from the route template.
    shape.parameters.push(Parameter {
        name: placeholders
            .first()
            .cloned()
            .unwrap_or_else(|| "param".into()),
        location: location.to_string(),
        required: true,
        schema: resolved,
    });
}

fn resolve_output(r: &mut Resolver, ctx: &Ctx, shape: &mut RouteShape, output: &Value) {
    if output.is_null() {
        shape.responses.insert("204".into(), Response::empty());
        return;
    }

    // `Result<T, E>` contributes both the success and the error contract.
    if output["resolved_path"]["path"].as_str() == Some("Result") {
        let args = all_args(&output["resolved_path"]);
        if let Some(ok) = args.first() {
            resolve_output(r, ctx, shape, ok);
        }
        if let Some(err) = args.get(1) {
            let schema = unwrap_body(r, ctx, err);
            shape
                .responses
                .insert("default".into(), Response::json(schema));
        }
        return;
    }

    // `(StatusCode, Json<T>)` — the code is a runtime value, so the body is
    // what the signature actually pins down.
    if let Some(elems) = output.get("tuple").and_then(|t| t.as_array()) {
        for elem in elems {
            if elem["resolved_path"]["path"]
                .as_str()
                .and_then(|p| ctx.extractors.role(p))
                .is_some_and(|role| matches!(role, Role::Body(_)))
            {
                resolve_output(r, ctx, shape, elem);
                return;
            }
        }
    }

    let content_type = output["resolved_path"]["path"]
        .as_str()
        .and_then(|p| ctx.extractors.role(p))
        .and_then(|role| match role {
            Role::Body(ct) => Some(ct.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "application/json".into());

    let schema = unwrap_body(r, ctx, output);
    shape.responses.insert(
        "200".into(),
        Response {
            content_type,
            schema,
            headers: Vec::new(),
            description: None,
        },
    );
}

/// Resolve a type, stepping through a body extractor wrapper if present.
fn unwrap_body(r: &mut Resolver, ctx: &Ctx, ty: &Value) -> Value {
    let is_body = ty["resolved_path"]["path"]
        .as_str()
        .and_then(|p| ctx.extractors.role(p))
        .is_some_and(|role| matches!(role, Role::Body(_)));
    let target = if is_body {
        first_arg(&ty["resolved_path"]).unwrap_or_else(|| ty.clone())
    } else {
        ty.clone()
    };
    r.resolve(&target, ctx.subst, ctx.at)
}

/// Apply attribute overrides, replacing whatever inference produced.
fn apply_overrides(r: &mut Resolver, shape: &mut RouteShape, o: &Overrides, at: &str) {
    if let Some(name) = &o.params {
        if let Some(schema) = resolve_named_in(r, name, at) {
            expand_query_override(r, shape, &schema);
        }
    }
    if let Some(name) = &o.request {
        if let Some(schema) = resolve_named_in(r, name, at) {
            shape.request = Some(RequestBody {
                content_type: shape
                    .request
                    .as_ref()
                    .map(|b| b.content_type.clone())
                    .unwrap_or_else(|| "application/json".into()),
                schema,
                required: true,
            });
        }
    }
    if let Some(name) = &o.response {
        if let Some(schema) = resolve_named_in(r, name, at) {
            let status = o
                .status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "200".into());
            // An override answers an erased return, so drop the gap it raised.
            r.gaps
                .retain(|g| !matches!(g, Gap::Erased { at: a, .. } if a == at));
            shape.responses.remove("200");
            shape.responses.insert(status, Response::json(schema));
            return;
        }
    }
    // A status override with no response override just relabels the success.
    if let Some(status) = o.status {
        if let Some(resp) = shape.responses.remove("200") {
            shape.responses.insert(status.to_string(), resp);
        }
    }
}

fn resolve_named_in(r: &mut Resolver, expr: &str, at: &str) -> Option<Value> {
    let expr = expr.trim();
    if let Some(inner) = expr.strip_prefix('[').and_then(|e| e.strip_suffix(']')) {
        let items = resolve_named_in(r, inner, at)?;
        return Some(json!({ "type": "array", "items": items }));
    }
    let id = r.find_type_id(expr)?;
    let node = json!({ "resolved_path": { "path": expr, "id": id, "args": null } });
    Some(r.resolve(&node, &Subst::new(), at))
}

/// Follow a `$ref` into the components map, if it is one.
fn deref(schema: &Value, components: &BTreeMap<String, Value>) -> Value {
    match schema["$ref"].as_str() {
        Some(r) => r
            .rsplit('/')
            .next()
            .and_then(|name| components.get(name))
            .cloned()
            .unwrap_or_else(|| schema.clone()),
        None => schema.clone(),
    }
}

/// `{id}` placeholders in a route template, in order.
fn path_placeholders(route_path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = route_path;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        out.push(rest[open + 1..open + close].to_string());
        rest = &rest[open + close + 1..];
    }
    out
}

fn first_arg(rp: &Value) -> Option<Value> {
    all_args(rp).into_iter().next()
}

fn all_args(rp: &Value) -> Vec<Value> {
    rp["args"]["angle_bracketed"]["args"]
        .as_array()
        .map(|args| args.iter().filter_map(|a| a.get("type").cloned()).collect())
        .unwrap_or_default()
}
