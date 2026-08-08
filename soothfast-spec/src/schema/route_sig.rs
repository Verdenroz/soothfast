//! Handler signatures → the wire contract they implement.
//!
//! A handler's parameters and return type already state what it accepts and
//! returns; restating that in attributes is the duplication this avoids. The
//! framework's extractor wrappers (`Json<T>`, `Query<T>`, ...) are what mark
//! which parameters are part of the contract and which are ambient context,
//! so they are matched through an extensible table rather than a fixed list —
//! an unlisted framework should cost a config line, not a code change.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::docs::Docs;
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
    /// Canonical paths walked out of a workspace-local crate — resolved
    /// rather than reported, and named only so a summary can say where the
    /// shapes came from.
    pub aux_resolved: BTreeSet<String>,
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
    ///
    /// A field naming one of the path's `{placeholder}`s types that path
    /// parameter instead of adding a query one — `r#type` and `type_` both
    /// name `{type}`, since a URL template carries no Rust escapes.
    pub params: Option<String>,
    /// Types for the path's `{placeholder}`s, as either `"name: Type"`
    /// bindings (comma-separated) or one struct name whose field names do
    /// the same job. Placeholders nothing names stay open strings.
    pub path_params: Option<String>,
}

impl Overrides {
    /// Whether these overrides alone are enough to build a
    /// [`detached_shape`] — a detached marker's whole contract is its
    /// attributes, so any one of them (not just `response`) is sufficient.
    fn is_detached_contract(&self) -> bool {
        self.request.is_some()
            || self.response.is_some()
            || self.params.is_some()
            || self.path_params.is_some()
    }
}

/// The route template's `{placeholder}`s and the schemas claimed for them.
///
/// Held apart from the shape's parameter list so path parameters can be
/// appended in *template* order at the end, whatever order they were typed
/// in — a struct's fields arrive sorted, and the URL's order is the one a
/// reader checks against.
#[derive(Debug, Default)]
struct PathTypes {
    names: Vec<String>,
    typed: BTreeMap<String, Value>,
}

impl PathTypes {
    fn new(route_path: &str) -> Self {
        PathTypes {
            names: path_placeholders(route_path),
            typed: BTreeMap::new(),
        }
    }

    /// The placeholder a name declares, and whether it spelled it exactly.
    ///
    /// Rust spells a keyword field `r#type` or `type_` where the URL template
    /// only ever says `{type}`, so those escapes are stripped for a second
    /// pass rather than forcing a `#[serde(rename)]` nobody wants.
    fn claimed_by(&self, name: &str) -> Option<(String, bool)> {
        if let Some(exact) = self.names.iter().find(|n| n.as_str() == name) {
            return Some((exact.clone(), true));
        }
        let bare = unescape(name);
        (!bare.is_empty())
            .then(|| self.names.iter().find(|n| unescape(n) == bare))
            .flatten()
            .map(|n| (n.clone(), false))
    }

    /// Type a placeholder, displacing anything already claimed for it.
    fn claim(&mut self, placeholder: &str, schema: Value) {
        self.typed.insert(placeholder.to_string(), schema);
    }

    /// Type a placeholder only if nothing firmer has. Keeps an exact name
    /// match winning over an escape-stripped one whatever order they arrive.
    fn claim_weak(&mut self, placeholder: &str, schema: Value) {
        self.typed.entry(placeholder.to_string()).or_insert(schema);
    }

    /// The schema for a placeholder — an open string when nothing typed it.
    fn schema(&self, placeholder: &str) -> Value {
        self.typed
            .get(placeholder)
            .cloned()
            .unwrap_or_else(|| json!({"type": "string"}))
    }
}

/// Strip the spellings Rust needs for a keyword identifier, which a URL
/// template never carries: `r#type` and `type_` both name `{type}`.
fn unescape(name: &str) -> &str {
    name.strip_prefix("r#")
        .unwrap_or(name)
        .trim_end_matches('_')
}

/// Infer the wire contract of one handler.
///
/// `handler_path` is the `#[route]` id (`app::routes::create_item`) and
/// `route_path` the URL template, whose `{placeholders}` name tuple path
/// parameters that the Rust signature leaves positional.
pub fn infer<'a>(
    docs: impl Into<Docs<'a>>,
    table: &'a TypeTable,
    extractors: &Extractors,
    handler_path: &str,
    route_path: &str,
    overrides: &Overrides,
) -> Result<RouteShape, String> {
    let mut r = Resolver::new(docs, table)?;
    let mut paths = PathTypes::new(route_path);
    let mut shape = match r.find_by_path(handler_path).cloned() {
        Some(item) => inferred_shape(
            &mut r,
            extractors,
            handler_path,
            overrides,
            &item,
            &mut paths,
        )?,
        None if overrides.is_detached_contract() => {
            detached_shape(&mut r, overrides, handler_path, &mut paths)?
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

    // Last, so an explicit `path_params` displaces whatever a params field
    // or a `Path<T>` extractor claimed for the same placeholder.
    resolve_path_params(&mut r, overrides, handler_path, &mut paths)?;
    synthesize_path_params(&mut shape, &paths);

    shape.components = r.components;
    shape.gaps = r.gaps;
    shape.aux_resolved = r.aux_resolved;
    Ok(shape)
}

/// Infer a shape from the handler's own rustdoc item, then apply overrides.
fn inferred_shape(
    r: &mut Resolver,
    extractors: &Extractors,
    handler_path: &str,
    overrides: &Overrides,
    item: &Value,
    paths: &mut PathTypes,
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
    let placeholders = paths.names.clone();
    let ctx = Ctx {
        extractors,
        placeholders: &placeholders,
        at: handler_path,
        subst: &subst,
    };

    for input in sig["inputs"].as_array().cloned().unwrap_or_default() {
        let name = input[0].as_str().unwrap_or("").to_string();
        classify_input(r, &ctx, &mut shape, &name, &input[1]);
    }

    resolve_output(r, &ctx, &mut shape, &sig["output"]);

    // Overrides win: they exist for shapes inference provably cannot see.
    apply_overrides(r, &mut shape, overrides, handler_path, paths);
    Ok(shape)
}

/// Build a shape purely from attribute overrides.
///
/// For a detached marker the attributes are the whole contract, so a type
/// name that cannot be resolved is an error here — the silent tolerance
/// [`apply_overrides`] extends to inferred shapes would leave an empty
/// operation behind a typo.
fn detached_shape(
    r: &mut Resolver,
    o: &Overrides,
    at: &str,
    paths: &mut PathTypes,
) -> Result<RouteShape, String> {
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
        if !expand_query_override(r, &mut shape, &schema, paths) {
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
///
/// A field naming one of the path's `{placeholder}`s types *that* parameter
/// instead of adding a query one: the URL template is the more specific
/// declaration, and emitting both would put one name on the wire twice. A
/// route that genuinely wants the same name in both places renames one side
/// (`#[serde(rename)]` on the field, or the placeholder).
///
/// Returns false when the schema has no named fields to expand.
fn expand_query_override(
    r: &Resolver,
    shape: &mut RouteShape,
    schema: &Value,
    paths: &mut PathTypes,
) -> bool {
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
        if let Some((placeholder, exact)) = paths.claimed_by(name) {
            match exact {
                true => paths.claim(&placeholder, schema.clone()),
                false => paths.claim_weak(&placeholder, schema.clone()),
            }
            continue;
        }
        shape.parameters.push(Parameter {
            name: name.clone(),
            location: "query".into(),
            required: required.contains(&name.as_str()),
            schema: schema.clone(),
        });
    }
    true
}

/// Resolve the `path_params` override into schemas keyed by placeholder.
///
/// Two spellings, told apart by whether an entry binds a name:
/// `"type: HolderType, statement: StatementType"` types listed placeholders
/// one at a time, and a bare `"ItemPath"` hands the job to a struct whose
/// field names do the same. Naming a placeholder the path doesn't have is an
/// error — it is always a typo, and silence would leave an open string
/// behind it.
fn resolve_path_params(
    r: &mut Resolver,
    o: &Overrides,
    at: &str,
    paths: &mut PathTypes,
) -> Result<(), String> {
    let Some(spec) = &o.path_params else {
        return Ok(());
    };
    let missing = |ty: &str| {
        format!(
            "path_params type `{ty}` for route `{at}` is not in this crate's \
             rustdoc JSON"
        )
    };
    for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let Some((name, ty)) = split_binding(entry) else {
            let schema = resolve_named_in(r, entry, at).ok_or_else(|| missing(entry))?;
            if !expand_path_struct(r, &schema, paths) {
                return Err(format!(
                    "path_params type `{entry}` for route `{at}` must be a struct \
                     with named fields, or a `name: Type` binding"
                ));
            }
            continue;
        };
        let Some((placeholder, _)) = paths.claimed_by(name) else {
            return Err(format!(
                "path_params names `{name}` for route `{at}`, which is not a \
                 `{{placeholder}}` in its path"
            ));
        };
        let schema = resolve_named_in(r, ty, at).ok_or_else(|| missing(ty))?;
        paths.claim(&placeholder, schema);
    }
    Ok(())
}

/// Type placeholders from a struct schema's fields. Fields naming no
/// placeholder are ignored — one path struct can serve several routes.
/// Returns false when the schema has no named fields to expand.
fn expand_path_struct(r: &Resolver, schema: &Value, paths: &mut PathTypes) -> bool {
    let object = deref(schema, &r.components);
    let Some(props) = object.get("properties").and_then(|p| p.as_object()) else {
        return false;
    };
    for (name, schema) in props {
        if let Some((placeholder, _)) = paths.claimed_by(name) {
            paths.claim(&placeholder, schema.clone());
        }
    }
    true
}

/// Split `"name: Type"` at the binding colon — the one that is not half of a
/// `::` path separator, so `sector: crate::Sector` reads as intended.
fn split_binding(entry: &str) -> Option<(&str, &str)> {
    let bytes = entry.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b':' {
            i += 1;
        } else if bytes.get(i + 1) == Some(&b':') {
            i += 2;
        } else {
            let (name, ty) = (entry[..i].trim(), entry[i + 1..].trim());
            return (!name.is_empty() && !ty.is_empty()).then_some((name, ty));
        }
    }
    None
}

/// Append a required path parameter for every `{placeholder}` in the route
/// template that nothing else declared, in template order, typed by whatever
/// claimed it and an open string otherwise.
fn synthesize_path_params(shape: &mut RouteShape, paths: &PathTypes) {
    for name in &paths.names {
        if let Some(declared) = shape
            .parameters
            .iter_mut()
            .find(|p| p.location == "path" && &p.name == name)
        {
            // An override still wins over what a `Path<T>` extractor typed.
            if let Some(schema) = paths.typed.get(name) {
                declared.schema = schema.clone();
            }
            continue;
        }
        shape.parameters.push(Parameter {
            name: name.clone(),
            location: "path".into(),
            required: true,
            schema: paths.schema(name),
        });
    }
}

/// Ambient inputs shared across one handler's inference.
struct Ctx<'c> {
    extractors: &'c Extractors,
    /// `{placeholder}` names in the route template, in order.
    placeholders: &'c [String],
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
    let placeholders = ctx.placeholders;

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
fn apply_overrides(
    r: &mut Resolver,
    shape: &mut RouteShape,
    o: &Overrides,
    at: &str,
    paths: &mut PathTypes,
) {
    if let Some(name) = &o.params {
        if let Some(schema) = resolve_named_in(r, name, at) {
            expand_query_override(r, shape, &schema, paths);
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
    r.resolve_by_name(expr, at)
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
