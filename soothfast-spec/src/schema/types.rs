//! Rustdoc type nodes → JSON Schema.
//!
//! Named local types resolve to `$ref`s into a components map, which both
//! keeps the emitted spec readable and makes self-referential types
//! terminate: a type already on the resolution stack yields its `$ref`
//! immediately and the outer frame fills the definition.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::foreign::TypeTable;
use super::{Gap, serde_attrs};

/// Generic parameter bindings captured at a concrete use site: parameter name
/// → (resolved schema, display name used for component naming).
pub type Subst = BTreeMap<String, (Value, String)>;

/// Walks rustdoc JSON, emitting JSON Schema and collecting [`Gap`]s.
pub struct Resolver<'a> {
    index: &'a serde_json::Map<String, Value>,
    paths: &'a serde_json::Map<String, Value>,
    table: &'a TypeTable,
    /// Named schemas, emitted as `components/schemas` in OpenAPI.
    pub components: BTreeMap<String, Value>,
    pub gaps: Vec<Gap>,
    /// Component names currently being resolved, to break reference cycles.
    stack: BTreeSet<String>,
}

impl<'a> Resolver<'a> {
    /// Build a resolver over a parsed rustdoc JSON document.
    pub fn new(doc: &'a Value, table: &'a TypeTable) -> Result<Self, String> {
        let index = doc["index"]
            .as_object()
            .ok_or("rustdoc JSON has no `index` object")?;
        let paths = doc["paths"]
            .as_object()
            .ok_or("rustdoc JSON has no `paths` object")?;
        Ok(Self {
            index,
            paths,
            table,
            components: BTreeMap::new(),
            gaps: Vec::new(),
            stack: BTreeSet::new(),
        })
    }

    pub(super) fn item(&self, id: &Value) -> Option<&'a Value> {
        self.index.get(&id_key(id)?)
    }

    /// Find a struct or enum by its plain name, for attribute overrides that
    /// name a type rather than pointing at a signature.
    pub fn find_type_id(&self, name: &str) -> Option<Value> {
        self.index
            .iter()
            .find(|(_, item)| {
                item["name"].as_str() == Some(name)
                    && (item["inner"].get("struct").is_some()
                        || item["inner"].get("enum").is_some())
            })
            .and_then(|(k, _)| k.parse::<u64>().ok().map(Value::from).or(Some(json!(k))))
    }

    /// Find an item by its fully-qualified path, e.g. `app::routes::create`.
    ///
    /// `#[route]` records handler ids as `module_path!()::fn_name`, which is
    /// exactly the path rustdoc reports, so route lookup is a direct match.
    pub fn find_by_path(&self, canonical: &str) -> Option<&'a Value> {
        let (id, _) = self.paths.iter().find(|(_, entry)| {
            entry["path"]
                .as_array()
                .map(|segs| {
                    let joined: Vec<&str> = segs.iter().filter_map(|s| s.as_str()).collect();
                    joined.join("::") == canonical
                })
                .unwrap_or(false)
        })?;
        self.index.get(id)
    }

    /// Canonical definition path of an id, e.g. `core::time::Duration`.
    pub(super) fn canonical(&self, id: &Value) -> Option<String> {
        let entry = self.paths.get(&id_key(id)?)?;
        let segs: Vec<&str> = entry["path"]
            .as_array()?
            .iter()
            .filter_map(|s| s.as_str())
            .collect();
        Some(segs.join("::"))
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
            _ => {}
        }

        let id = &rp["id"];
        // A local type is in `index` and can be walked; a foreign one is only
        // in `paths`, so it is identifiable but opaque.
        if self.item(id).is_some() {
            return self.resolve_local(id, &args, subst, at);
        }
        let canonical = self.canonical(id).unwrap_or_else(|| display.to_string());
        if let Some(mapped) = self.table.lookup(&canonical) {
            return mapped.clone();
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

    /// Resolve a local named type to a `$ref`, defining the component on
    /// first visit.
    fn resolve_local(&mut self, id: &Value, args: &[Value], subst: &Subst, at: &str) -> Value {
        let Some(item) = self.item(id) else {
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
        let component = if arg_names.is_empty() {
            base
        } else {
            format!("{base}_{}", arg_names.join("_"))
        };
        let reference = json!({ "$ref": format!("#/components/schemas/{component}") });

        if self.components.contains_key(&component) || self.stack.contains(&component) {
            return reference;
        }
        self.stack.insert(component.clone());
        // Reserve the name before descending so a cycle finds it on the stack.
        let defined = self.define(item, &inner_subst, &component);
        self.stack.remove(&component);
        self.components.insert(component, defined);
        reference
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
        if let Some(doc) = item["docs"].as_str().filter(|d| !d.is_empty()) {
            if let Some(obj) = schema.as_object_mut() {
                let first = doc.split("\n\n").next().unwrap_or(doc);
                obj.insert("description".into(), json!(first.trim()));
            }
        }
    }

    /// Container-level serde attributes, reporting an unusable rename rule.
    pub(super) fn container_attrs(
        &mut self,
        item: &Value,
        at: &str,
    ) -> serde_attrs::ContainerAttrs {
        let empty = Vec::new();
        let attrs = item["attrs"].as_array().unwrap_or(&empty);
        let c = serde_attrs::container(attrs);
        if let Some(rule) = &c.unknown_rename_all {
            self.record(Gap::UnknownRenameRule {
                at: at.to_string(),
                rule: rule.clone(),
            });
        }
        c
    }
}

/// Rustdoc ids appear as integers or strings depending on version.
fn id_key(id: &Value) -> Option<String> {
    match id {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// The angle-bracketed type arguments of a resolved path, in order.
fn generic_args(rp: &Value) -> Vec<Value> {
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
