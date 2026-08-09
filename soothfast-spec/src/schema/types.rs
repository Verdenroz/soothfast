//! Rustdoc type nodes → JSON Schema.
//!
//! Named local types resolve to `$ref`s into a components map, which both
//! keeps the emitted spec readable and makes self-referential types
//! terminate: a type already on the resolution stack yields its `$ref`
//! immediately and the outer frame fills the definition.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::docs::{Doc, Docs};
use super::foreign::{TypeMapping, TypeTable};
use super::naming;
use super::wire_names::WireNames;
use super::{Gap, graphql_attrs, serde_attrs};

/// Generic parameter bindings captured at a concrete use site: parameter name
/// → (resolved schema, display name used for component naming).
pub type Subst = BTreeMap<String, (Value, String)>;

/// Walks rustdoc JSON, emitting JSON Schema and collecting [`Gap`]s.
///
/// Resolution may span several documents (see [`Docs`]), but `components`,
/// the component-name registry and the cycle-breaking stack are shared across
/// all of them: one type gets one component wherever it was defined.
pub struct Resolver<'a> {
    /// The documents in consultation order; `docs[0]` is the primary one.
    docs: Vec<Doc<'a>>,
    /// Which document the current descent is walking. A type pulled out of an
    /// auxiliary document names its fields' types in that same document, so
    /// the cursor moves with the descent and is restored on the way out.
    cur: usize,
    table: &'a TypeTable,
    /// Named schemas, emitted as `components/schemas` in OpenAPI.
    pub components: BTreeMap<String, Value>,
    /// Canonical path → component-name stem, decided up front from the
    /// documents alone (see [`naming`]). Because it does not depend on the
    /// walk, every resolver over the same documents names a type identically,
    /// which is what lets a dialect merge many operations' components.
    stems: BTreeMap<String, String>,
    /// Component name → the type identity holding it. The stems already keep
    /// distinct types apart; this catches what they cannot see — a type with
    /// no canonical path at all, and a monomorphisation suffix that happens
    /// to spell another type's name.
    owners: BTreeMap<String, String>,
    pub gaps: Vec<Gap>,
    /// Canonical paths walked out of an auxiliary document. Resolved, so
    /// deliberately *not* gaps — reported only to say where they came from.
    pub aux_resolved: BTreeSet<String>,
    /// Component names currently being resolved, to break reference cycles.
    stack: BTreeSet<String>,
}

impl<'a> Resolver<'a> {
    /// Build a resolver over a parsed rustdoc JSON document, or over a
    /// primary document plus its workspace-local companions ([`Docs`]).
    pub fn new(docs: impl Into<Docs<'a>>, table: &'a TypeTable) -> Result<Self, String> {
        let docs = docs.into();
        let mut open = vec![Doc::open(docs.primary())?];
        open.extend(docs.aux().iter().filter_map(|d| Doc::open(d).ok()));
        let walkable: BTreeSet<String> = open
            .iter()
            .flat_map(Doc::walkable)
            .map(String::clone)
            .collect();
        Ok(Self {
            stems: naming::stems(&walkable),
            docs: open,
            cur: 0,
            table,
            components: BTreeMap::new(),
            owners: BTreeMap::new(),
            gaps: Vec::new(),
            aux_resolved: BTreeSet::new(),
            stack: BTreeSet::new(),
        })
    }

    pub(super) fn item(&self, id: &Value) -> Option<&'a Value> {
        self.docs[self.cur].item(id)
    }

    /// Find a struct or enum by its plain name, for attribute overrides that
    /// name a type rather than pointing at a signature.
    pub fn find_type_id(&self, name: &str) -> Option<Value> {
        self.docs[0].scan_type_id(name)
    }

    /// Find an item by its fully-qualified path, e.g. `app::routes::create`.
    ///
    /// `#[route]` records handler ids as `module_path!()::fn_name`, which is
    /// exactly the path rustdoc reports, so route lookup is a direct match.
    pub fn find_by_path(&self, canonical: &str) -> Option<&'a Value> {
        let doc = &self.docs[0];
        let (id, _) = doc.paths.iter().find(|(_, entry)| {
            entry["path"]
                .as_array()
                .map(|segs| {
                    let joined: Vec<&str> = segs.iter().filter_map(|s| s.as_str()).collect();
                    joined.join("::") == canonical
                })
                .unwrap_or(false)
        })?;
        doc.index.get(id)
    }

    /// Resolve a struct or enum named in an attribute, searching this crate
    /// first and then the workspace-local documents.
    pub fn resolve_by_name(&mut self, name: &str, at: &str) -> Option<Value> {
        if let Some(id) = self.find_type_id(name) {
            let node = json!({ "resolved_path": { "path": name, "id": id, "args": null } });
            return Some(self.resolve(&node, &Subst::new(), at));
        }
        let found =
            (1..self.docs.len()).find_map(|i| self.docs[i].locate_name(name).map(|id| (i, id)));
        let (home, id) = found?;
        self.note_aux(home, &id);
        Some(self.resolve_named(home, &id, &[], &Subst::new(), at))
    }

    /// Canonical definition path of an id, in the document being walked.
    pub(super) fn canonical(&self, id: &Value) -> Option<String> {
        self.docs[self.cur].canonical(id)
    }

    /// Record that a type came out of an auxiliary crate rather than a gap.
    fn note_aux(&mut self, home: usize, id: &Value) {
        if let Some(path) = self.docs[home].canonical(id) {
            self.aux_resolved.insert(path);
        }
    }

    pub(super) fn record(&mut self, gap: Gap) {
        if !self.gaps.contains(&gap) {
            self.gaps.push(gap);
        }
    }

    /// True when a type is `Option<..>`, which makes a field optional on the
    /// wire independently of the schema its inner type produces.
    pub fn is_option(ty: &Value) -> bool {
        ty["resolved_path"]["path"]
            .as_str()
            .is_some_and(|p| p == "Option" || p.ends_with("::Option"))
    }

    /// Resolve one rustdoc type node into a JSON Schema.
    ///
    /// `at` is a human-readable location (`Item.when`) used for gap reports.
    pub fn resolve(&mut self, ty: &Value, subst: &Subst, at: &str) -> Value {
        if let Some(p) = ty.get("primitive").and_then(|v| v.as_str()) {
            return primitive(p);
        }
        if let Some(name) = ty.get("generic").and_then(|v| v.as_str()) {
            // An unbound parameter means the use site was not monomorphic.
            return match subst.get(name) {
                Some((schema, _)) => schema.clone(),
                None => json!({}),
            };
        }
        if let Some(bounds) = ty.get("impl_trait") {
            return self.erased(bounds, at);
        }
        if let Some(dy) = ty.get("dyn_trait") {
            return self.erased(&dy["traits"], at);
        }
        if let Some(inner) = ty.get("borrowed_ref") {
            return self.resolve(&inner["type"], subst, at);
        }
        if let Some(inner) = ty.get("slice") {
            let items = self.resolve(inner, subst, at);
            return json!({ "type": "array", "items": items });
        }
        if let Some(arr) = ty.get("array") {
            let items = self.resolve(&arr["type"], subst, at);
            let mut schema = json!({ "type": "array", "items": items });
            if let Some(len) = arr["len"].as_str().and_then(|l| l.parse::<u64>().ok()) {
                schema["minItems"] = json!(len);
                schema["maxItems"] = json!(len);
            }
            return schema;
        }
        if let Some(elems) = ty.get("tuple").and_then(|v| v.as_array()) {
            // serde emits the unit tuple as null and others as a JSON array.
            if elems.is_empty() {
                return json!({ "type": "null" });
            }
            let items: Vec<Value> = elems.iter().map(|e| self.resolve(e, subst, at)).collect();
            let n = items.len();
            return json!({ "type": "array", "prefixItems": items,
                           "minItems": n, "maxItems": n });
        }
        if ty.get("resolved_path").is_some() {
            return self.resolve_path(&ty["resolved_path"], subst, at);
        }
        // Function pointers, qualified paths and inferred types have no wire
        // meaning worth guessing at.
        json!({})
    }

    fn erased(&mut self, bounds: &Value, at: &str) -> Value {
        let bound = bounds
            .as_array()
            .and_then(|b| b.first())
            .and_then(|b| b["trait_bound"]["trait"]["path"].as_str())
            .unwrap_or("?")
            .to_string();
        self.record(Gap::Erased {
            at: at.to_string(),
            bound,
        });
        json!({})
    }

    fn resolve_path(&mut self, rp: &Value, subst: &Subst, at: &str) -> Value {
        let display = rp["path"].as_str().unwrap_or_default();
        let args = generic_args(rp);
        let last = display.rsplit("::").next().unwrap_or(display);

        // Standard containers are structural: they shape the schema rather
        // than contributing a named component of their own.
        match last {
            "Option" => {
                // Optionality is carried by the field, not the schema; a
                // nullable inner schema is the honest rendering elsewhere.
                return match args.first() {
                    Some(inner) => self.resolve(inner, subst, at),
                    None => json!({}),
                };
            }
            "Box" | "Arc" | "Rc" | "Cow" | "RefCell" | "Cell" | "Mutex" | "RwLock" => {
                return match args.first() {
                    Some(inner) => self.resolve(inner, subst, at),
                    None => json!({}),
                };
            }
            "Vec" | "VecDeque" | "HashSet" | "BTreeSet" | "BinaryHeap" | "LinkedList" => {
                let items = match args.first() {
                    Some(inner) => self.resolve(inner, subst, at),
                    None => json!({}),
                };
                let mut schema = json!({ "type": "array", "items": items });
                if matches!(last, "HashSet" | "BTreeSet") {
                    schema["uniqueItems"] = json!(true);
                }
                return schema;
            }
            "HashMap" | "BTreeMap" => {
                let values = match args.get(1) {
                    Some(v) => self.resolve(v, subst, at),
                    None => json!({}),
                };
                return json!({ "type": "object", "additionalProperties": values });
            }
            "Result" => {
                // serde's externally tagged representation of Result.
                let ok = args
                    .first()
                    .map(|t| self.resolve(t, subst, at))
                    .unwrap_or(json!({}));
                let err = args
                    .get(1)
                    .map(|t| self.resolve(t, subst, at))
                    .unwrap_or(json!({}));
                return json!({ "oneOf": [
                    { "type": "object", "properties": { "Ok": ok },
                      "required": ["Ok"], "additionalProperties": false },
                    { "type": "object", "properties": { "Err": err },
                      "required": ["Err"], "additionalProperties": false },
                ]});
            }
            // async-graphql's cursor-pagination wrapper — hand-written
            // (`#[Object]` inside async-graphql itself, not derived from its
            // own struct fields), so it can never be walked out of rustdoc
            // JSON like a local type; its wire shape is fixed by the crate's
            // own resolver, not by anything this repo defines. `Cursor` (the
            // first argument) never reaches the wire itself — only through
            // the `cursor` field the crate renders as a string — so only the
            // second argument, the node type, needs resolving.
            "Connection" => {
                let node = args
                    .get(1)
                    .map(|t| self.resolve(t, subst, at))
                    .unwrap_or(json!({}));
                return json!({
                    "type": "object",
                    "properties": {
                        "pageInfo": {
                            "type": "object",
                            "properties": {
                                "hasPreviousPage": { "type": "boolean" },
                                "hasNextPage": { "type": "boolean" },
                                "startCursor": { "type": ["string", "null"] },
                                "endCursor": { "type": ["string", "null"] },
                            },
                            "required": ["hasPreviousPage", "hasNextPage"],
                        },
                        "edges": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": { "cursor": { "type": "string" }, "node": node.clone() },
                                "required": ["cursor", "node"],
                            },
                        },
                        "nodes": { "type": "array", "items": node },
                    },
                    "required": ["pageInfo", "edges", "nodes"],
                });
            }
            _ => {}
        }

        let id = &rp["id"];
        // A type in the current document's `index` can be walked; one only in
        // `paths` is identifiable but opaque *here*.
        if self.item(id).is_some() {
            return self.resolve_named(self.cur, id, &args, subst, at);
        }
        let canonical = self.canonical(id).unwrap_or_else(|| display.to_string());
        // The table first: an explicit mapping is the escape hatch for a type
        // soothfast would derive wrongly, so it outranks anything derived.
        match self.table.lookup(&canonical).cloned() {
            Some(TypeMapping::Literal(schema)) => return schema,
            Some(TypeMapping::Transparent { arg }) => {
                return self.transparent(&canonical, arg, &args, subst, at);
            }
            None => {}
        }
        // Then the workspace: a sibling crate's type is as walkable as a local
        // one once its own document is on hand.
        if let Some((home, aux_id)) = self.locate_aux(&canonical) {
            self.aux_resolved.insert(canonical);
            return self.resolve_named(home, &aux_id, &args, subst, at);
        }
        if last == "String" || last == "str" {
            return json!({ "type": "string" });
        }
        self.record(Gap::UnmappedForeign {
            at: at.to_string(),
            path: canonical,
        });
        json!({})
    }

    /// Resolve a wrapper mapped `transparent`: its wire shape is exactly its
    /// type argument's, so it contributes no component and no `$ref` of its
    /// own — the argument's schema stands in its place.
    fn transparent(
        &mut self,
        path: &str,
        arg: usize,
        args: &[Value],
        subst: &Subst,
        at: &str,
    ) -> Value {
        match args.get(arg) {
            Some(inner) => self.resolve(inner, subst, at),
            None => {
                // Nothing to forward to: a guess here would invent a shape.
                self.record(Gap::TransparentWithoutArgument {
                    at: at.to_string(),
                    path: path.to_string(),
                    arg,
                });
                json!({})
            }
        }
    }

    /// The first auxiliary document defining `canonical`, and its id there.
    fn locate_aux(&self, canonical: &str) -> Option<(usize, Value)> {
        (1..self.docs.len()).find_map(|i| self.docs[i].locate(canonical).map(|id| (i, id)))
    }

    /// Resolve a named type living in `home` to a `$ref`, defining the
    /// component on first visit.
    ///
    /// Generic arguments come from the *use site*, so they are resolved
    /// before the cursor moves; only the body walk happens in `home`.
    fn resolve_named(
        &mut self,
        home: usize,
        id: &Value,
        args: &[Value],
        subst: &Subst,
        at: &str,
    ) -> Value {
        let Some(item) = self.docs[home].item(id) else {
            return json!({});
        };
        let base = item["name"].as_str().unwrap_or("Anonymous").to_string();

        // Bind the type's own generic parameters to this use site's args.
        let params = generic_param_names(item);
        let mut inner_subst = Subst::new();
        let mut arg_names = Vec::new();
        for (i, param) in params.iter().enumerate() {
            let Some(arg) = args.get(i) else { continue };
            let schema = self.resolve(arg, subst, at);
            let name = type_display_name(arg, subst);
            arg_names.push(name.clone());
            inner_subst.insert(param.clone(), (schema, name));
        }
        let suffix = |stem: &str| match arg_names.is_empty() {
            true => stem.to_string(),
            false => format!("{stem}_{}", arg_names.join("_")),
        };
        // The stem is a property of the type, so every operation in the
        // document names it the same way and their components can be merged.
        let canonical = self.docs[home].canonical(id);
        let stem = canonical
            .as_ref()
            .and_then(|c| self.stems.get(c))
            .unwrap_or(&base);
        let identity = suffix(canonical.as_ref().unwrap_or(&base));
        let component = self.claim(&suffix(stem), &identity);
        let reference = json!({ "$ref": format!("#/components/schemas/{component}") });

        if self.components.contains_key(&component) || self.stack.contains(&component) {
            return reference;
        }
        self.stack.insert(component.clone());
        // Reserve the name before descending so a cycle finds it on the stack.
        let outer = std::mem::replace(&mut self.cur, home);
        let defined = self.define(item, &inner_subst, &component);
        self.cur = outer;
        self.stack.remove(&component);
        self.components.insert(component, defined);
        reference
    }

    /// Hold `display` for this type, or number it if another type has it.
    ///
    /// [`naming::stems`] has already told apart every type the documents can
    /// see, so this only fires for the two things it cannot: a type rustdoc
    /// gives no canonical path (identified by bare name alone, exactly as
    /// before), and a monomorphisation suffix that happens to spell another
    /// component's name. A name is never re-pointed at a second type.
    fn claim(&mut self, display: &str, identity: &str) -> String {
        let mut candidate = display.to_string();
        let mut n = 1;
        loop {
            match self.owners.get(&candidate) {
                Some(owner) if owner == identity => return candidate,
                // `owners` is finite, so a free name is always reached.
                Some(_) => {
                    n += 1;
                    candidate = format!("{display}_{n}");
                }
                None => {
                    self.owners.insert(candidate.clone(), identity.to_string());
                    return candidate;
                }
            }
        }
    }

    /// Build the schema body for a local struct or enum.
    fn define(&mut self, item: &'a Value, subst: &Subst, at: &str) -> Value {
        let inner = &item["inner"];
        if let Some(s) = inner.get("struct") {
            return self.walk_struct(item, s, subst, at);
        }
        if let Some(e) = inner.get("enum") {
            return self.walk_enum(item, e, subst, at);
        }
        if let Some(t) = inner.get("type_alias") {
            return self.resolve(&t["type"], subst, at);
        }
        json!({})
    }

    /// Attach a `description` from the item's own doc comment when present.
    pub(super) fn describe(item: &Value, schema: &mut Value) {
        if let Some(doc) = item["docs"].as_str().filter(|d| !d.is_empty())
            && let Some(obj) = schema.as_object_mut()
        {
            let first = doc.split("\n\n").next().unwrap_or(doc);
            obj.insert("description".into(), json!(first.trim()));
        }
    }

    /// Container-level serde attributes, reporting an unusable rename rule.
    pub(super) fn container_attrs(&mut self, item: &Value, at: &str) -> WireNames {
        let empty = Vec::new();
        let attrs = item["attrs"].as_array().unwrap_or(&empty);
        let serde = serde_attrs::container(attrs);

        // A GraphQL-served type is known two ways: by the container traits its
        // derive generated, and — for documents whose impls rustdoc did not
        // record — by carrying a `#[graphql(...)]` attribute at all.
        let gql = graphql_attrs::container(attrs);
        let served = gql.present || self.docs[self.cur].serves_graphql(item);
        let unknown = match served {
            true => gql.unknown_rename_rule.clone(),
            false => serde.unknown_rename_all.clone(),
        };
        if let Some(rule) = unknown {
            self.record(Gap::UnknownRenameRule {
                at: at.to_string(),
                rule,
            });
        }
        WireNames {
            serde,
            graphql: served.then_some(gql),
        }
    }
}

/// The angle-bracketed type arguments of a resolved path, in order.
pub(super) fn generic_args(rp: &Value) -> Vec<Value> {
    rp["args"]["angle_bracketed"]["args"]
        .as_array()
        .map(|args| args.iter().filter_map(|a| a.get("type").cloned()).collect())
        .unwrap_or_default()
}

/// Names of a type's own generic parameters, in declaration order.
fn generic_param_names(item: &Value) -> Vec<String> {
    let inner = &item["inner"];
    let generics = inner
        .get("struct")
        .or_else(|| inner.get("enum"))
        .or_else(|| inner.get("type_alias"))
        .map(|k| &k["generics"]);
    generics
        .and_then(|g| g["params"].as_array())
        .map(|ps| {
            ps.iter()
                .filter(|p| p["kind"].get("type").is_some())
                .filter_map(|p| p["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// A short display name for a type argument, used to name monomorphised
/// components (`ApiResponse<Item>` → `ApiResponse_Item`).
fn type_display_name(ty: &Value, subst: &Subst) -> String {
    if let Some(p) = ty.get("primitive").and_then(|v| v.as_str()) {
        return p.to_string();
    }
    if let Some(g) = ty.get("generic").and_then(|v| v.as_str()) {
        return subst
            .get(g)
            .map(|(_, n)| n.clone())
            .unwrap_or_else(|| g.to_string());
    }
    if let Some(path) = ty["resolved_path"]["path"].as_str() {
        let base = path.rsplit("::").next().unwrap_or(path).to_string();
        let args = generic_args(&ty["resolved_path"]);
        if args.is_empty() {
            return base;
        }
        let inner: Vec<String> = args.iter().map(|a| type_display_name(a, subst)).collect();
        return format!("{base}_{}", inner.join("_"));
    }
    "T".to_string()
}

/// Rust primitive → JSON Schema, with the integer formats OpenAPI expects.
fn primitive(p: &str) -> Value {
    match p {
        "bool" => json!({ "type": "boolean" }),
        "char" => json!({ "type": "string", "minLength": 1, "maxLength": 1 }),
        "str" => json!({ "type": "string" }),
        "f32" => json!({ "type": "number", "format": "float" }),
        "f64" => json!({ "type": "number", "format": "double" }),
        "i8" | "i16" | "i32" => json!({ "type": "integer", "format": "int32" }),
        "u8" | "u16" | "u32" => json!({ "type": "integer", "format": "int32", "minimum": 0 }),
        "i64" | "i128" | "isize" => json!({ "type": "integer", "format": "int64" }),
        "u64" | "u128" | "usize" => {
            json!({ "type": "integer", "format": "int64", "minimum": 0 })
        }
        "()" | "never" => json!({ "type": "null" }),
        _ => json!({}),
    }
}
