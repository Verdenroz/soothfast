//! Rustdoc type nodes → the binding type lattice.
//!
//! Bindings name an exported type rather than inlining it, so resolution is
//! a single pass with no component registry and no cycle-breaking stack: a
//! self-referential type yields `Ty::Class` on sight and the walk stops there.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{Map, Value};

use crate::foreign::TypeTable;
use crate::gap::Gap;
use crate::model::Ty;

/// Containers that bind as an ordered sequence.
const SEQUENCES: &[&str] = &[
    "Vec",
    "VecDeque",
    "HashSet",
    "BTreeSet",
    "BinaryHeap",
    "LinkedList",
];

/// Walks rustdoc JSON, producing [`Ty`] and collecting [`Gap`]s.
pub struct Resolver<'a> {
    index: &'a Map<String, Value>,
    paths: &'a Map<String, Value>,
    table: &'a TypeTable,
    /// Canonical paths of the package's exported types. Anything else is a
    /// table lookup or a gap.
    exported: BTreeSet<String>,
    /// The type `Self` stands for in the item being walked.
    self_ty: Option<String>,
    /// Item id → the shortest path a downstream crate can name it by.
    public: BTreeMap<String, String>,
    pub gaps: Vec<Gap>,
}

impl<'a> Resolver<'a> {
    /// Build a resolver over a parsed rustdoc JSON document.
    pub fn new(
        doc: &'a Value,
        table: &'a TypeTable,
        exported: BTreeSet<String>,
    ) -> Result<Self, String> {
        let index = doc["index"]
            .as_object()
            .ok_or("rustdoc JSON has no `index` object")?;
        let paths = doc["paths"]
            .as_object()
            .ok_or("rustdoc JSON has no `paths` object")?;
        Ok(Resolver {
            index,
            paths,
            table,
            exported,
            self_ty: None,
            public: public_paths(doc, index),
            gaps: Vec::new(),
        })
    }

    /// The path a glue crate can spell an item by: through public modules
    /// and `pub use` re-exports only, since the module an item is defined
    /// in is often private. `None` when nothing public reaches it, or when
    /// the document carries no module tree to walk.
    pub fn public_path(&self, id: &Value) -> Option<String> {
        self.public.get(&id_key(id)?).cloned()
    }

    /// Set the type `Self` resolves to for the item about to be walked.
    pub fn enter(&mut self, owner: Option<&str>) {
        self.self_ty = owner.map(ToString::to_string);
    }

    /// The item an id points at.
    pub fn item(&self, id: &Value) -> Option<&'a Value> {
        self.index.get(&id_key(id)?)
    }

    /// Canonical definition path of an id.
    pub fn canonical(&self, id: &Value) -> Option<String> {
        let segments = self.paths.get(&id_key(id)?)?["path"].as_array()?;
        Some(join_path(segments))
    }

    /// Find an item by its fully-qualified path.
    ///
    /// `#[export]` records ids as `module_path!()` plus the item name, which
    /// is exactly the path rustdoc reports, so lookup is a direct match.
    pub fn find_by_path(&self, canonical: &str) -> Option<&'a Value> {
        let (id, _) = self.paths.iter().find(|(_, entry)| {
            entry["path"]
                .as_array()
                .is_some_and(|segs| join_path(segs) == canonical)
        })?;
        self.index.get(id)
    }

    pub(crate) fn record(&mut self, gap: Gap) {
        if !self.gaps.contains(&gap) {
            self.gaps.push(gap);
        }
    }

    /// Resolve a type that crosses as a message rather than as a value.
    ///
    /// An error type is rendered through `Display`, so it needs no mapping
    /// and its absence from the table is not a gap.
    pub fn resolve_message(&mut self, ty: &Value, at: &str) -> Ty {
        self.resolve_unreported(ty, at)
    }

    /// Resolve a type nothing reads across the boundary, such as a private
    /// field: what it holds never has to cross, so it is not a gap either.
    pub fn resolve_unreported(&mut self, ty: &Value, at: &str) -> Ty {
        let before = self.gaps.len();
        let out = self.resolve(ty, at);
        self.gaps.truncate(before);
        out
    }

    /// Resolve a parameter type. `impl Into<T>` and `impl AsRef<str>` in
    /// argument position bind as the type a caller would pass: the concrete
    /// argument the glue hands over satisfies the bound on its own, so the
    /// call site needs nothing extra.
    pub fn resolve_param(&mut self, ty: &Value, at: &str) -> Ty {
        match ty.get("impl_trait").and_then(accepted_argument) {
            Some(target) => self.resolve(&target, at),
            None => self.resolve(ty, at),
        }
    }

    /// A `type` alias's target with the given arguments substituted for its
    /// parameters, or `None` for a path that is not an alias in this
    /// package's index. `type Result<T> = std::result::Result<T, Error>`
    /// reads as the two-armed `Result` it names.
    pub(crate) fn expand_alias(&self, rp: &Value) -> Option<Value> {
        let alias = self.item(&rp["id"])?["inner"].get("type_alias")?;
        let args = generic_args(rp);
        let bindings: BTreeMap<&str, Value> = alias["generics"]["params"]
            .as_array()?
            .iter()
            .filter(|p| p["kind"].get("type").is_some())
            .enumerate()
            .filter_map(|(n, p)| {
                let name = p["name"].as_str()?;
                let arg = args.get(n).cloned().or_else(|| {
                    p["kind"]["type"]["default"]
                        .as_object()
                        .map(|_| p["kind"]["type"]["default"].clone())
                })?;
                Some((name, arg))
            })
            .collect();
        Some(substitute(&alias["type"], &bindings))
    }

    /// Resolve one rustdoc type node.
    ///
    /// `at` is the item the type appeared in, used for gap reports.
    pub fn resolve(&mut self, ty: &Value, at: &str) -> Ty {
        if let Some(p) = ty.get("primitive").and_then(Value::as_str) {
            return self.primitive(p, at);
        }
        if let Some(name) = ty.get("generic").and_then(Value::as_str) {
            if name == "Self"
                && let Some(owner) = self.self_ty.clone()
            {
                return Ty::Class(owner);
            }
            self.record(Gap::Generic {
                at: at.to_string(),
                param: name.to_string(),
            });
            return Ty::Opaque(name.to_string());
        }
        if let Some(bounds) = ty.get("impl_trait") {
            return self.erased(bounds, at);
        }
        if let Some(dy) = ty.get("dyn_trait") {
            return self.erased(&dy["traits"], at);
        }
        if let Some(inner) = ty.get("borrowed_ref") {
            return self.resolve(&inner["type"], at);
        }
        if let Some(inner) = ty.get("slice") {
            return self.sequence(inner, at);
        }
        if let Some(arr) = ty.get("array") {
            return self.sequence(&arr["type"], at);
        }
        if let Some(elems) = ty.get("tuple").and_then(Value::as_array) {
            if elems.is_empty() {
                return Ty::Unit;
            }
            return Ty::Tuple(elems.iter().map(|e| self.resolve(e, at)).collect());
        }
        if ty.get("resolved_path").is_some() {
            return self.resolve_path(&ty["resolved_path"], at);
        }
        let rendered = render(ty);
        self.record(Gap::UnmappedForeign {
            at: at.to_string(),
            path: rendered.clone(),
        });
        Ty::Opaque(rendered)
    }

    /// `Vec<u8>` and `&[u8]` bind as bytes, never as a list of numbers.
    fn sequence(&mut self, inner: &Value, at: &str) -> Ty {
        match self.resolve(inner, at) {
            Ty::U8 => Ty::Bytes,
            item => Ty::List(Box::new(item)),
        }
    }

    fn erased(&mut self, bounds: &Value, at: &str) -> Ty {
        let bound = bounds
            .as_array()
            .and_then(|b| b.first())
            .and_then(|b| b["trait_bound"]["trait"]["path"].as_str())
            .unwrap_or("?")
            .to_string();
        self.record(Gap::Erased {
            at: at.to_string(),
            bound: bound.clone(),
        });
        Ty::Opaque(bound)
    }

    fn primitive(&mut self, p: &str, at: &str) -> Ty {
        match p {
            "bool" => Ty::Bool,
            "str" => Ty::Str,
            "f32" => Ty::F32,
            "f64" => Ty::F64,
            "i8" => Ty::I8,
            "i16" => Ty::I16,
            "i32" => Ty::I32,
            "i64" => Ty::I64,
            "isize" => Ty::ISize,
            "u8" => Ty::U8,
            "u16" => Ty::U16,
            "u32" => Ty::U32,
            "u64" => Ty::U64,
            "usize" => Ty::USize,
            "()" | "never" => Ty::Unit,
            other => {
                self.record(Gap::UnmappedForeign {
                    at: at.to_string(),
                    path: other.to_string(),
                });
                Ty::Opaque(other.to_string())
            }
        }
    }

    fn resolve_path(&mut self, rp: &Value, at: &str) -> Ty {
        if let Some(target) = self.expand_alias(rp) {
            return self.resolve(&target, at);
        }
        let display = rp["path"].as_str().unwrap_or_default();
        let args = generic_args(rp);
        let last = display.rsplit("::").next().unwrap_or(display);

        match last {
            "Option" => {
                let inner = self.arg(&args, 0, at);
                return Ty::Optional(Box::new(inner));
            }
            "String" | "str" => return Ty::Str,
            _ if SEQUENCES.contains(&last) => {
                let item = self.arg(&args, 0, at);
                return match item {
                    Ty::U8 if last == "Vec" => Ty::Bytes,
                    item => Ty::List(Box::new(item)),
                };
            }
            "HashMap" | "BTreeMap" => {
                let key = self.arg(&args, 0, at);
                let value = self.arg(&args, 1, at);
                return Ty::Map(Box::new(key), Box::new(value));
            }
            _ => {}
        }

        let canonical = self
            .canonical(&rp["id"])
            .unwrap_or_else(|| display.to_string());
        if self.exported.contains(&canonical) {
            return Ty::Class(last.to_string());
        }
        if let Some(mapped) = self.table.lookup(&canonical) {
            return mapped.clone();
        }
        self.record(Gap::UnmappedForeign {
            at: at.to_string(),
            path: canonical.clone(),
        });
        Ty::Opaque(canonical)
    }

    /// One generic argument, or an opaque placeholder when it is absent.
    fn arg(&mut self, args: &[Value], n: usize, at: &str) -> Ty {
        match args.get(n) {
            Some(ty) => self.resolve(ty, at),
            None => Ty::Opaque("_".into()),
        }
    }
}

/// The angle-bracketed type arguments of a resolved path, in order.
pub(crate) fn generic_args(rp: &Value) -> Vec<Value> {
    rp["args"]["angle_bracketed"]["args"]
        .as_array()
        .map(|args| args.iter().filter_map(|a| a.get("type").cloned()).collect())
        .unwrap_or_default()
}

/// The argument type an `impl Trait` parameter takes outright: `T` for
/// `impl Into<T>`, `str` for `impl AsRef<str>`, `[u8]` for
/// `impl AsRef<[u8]>`. Any other bound stays erased.
fn accepted_argument(bounds: &Value) -> Option<Value> {
    let [bound] = bounds.as_array()?.as_slice() else {
        return None;
    };
    let tr = &bound["trait_bound"]["trait"];
    let name = tr["path"].as_str()?.rsplit("::").next()?;
    let arg = generic_args(tr).into_iter().next()?;
    let as_ref_target = arg["primitive"].as_str() == Some("str")
        || arg["slice"]["primitive"].as_str() == Some("u8");
    match name {
        "Into" => Some(arg),
        "AsRef" if as_ref_target => Some(arg),
        _ => None,
    }
}

/// `ty` with every `{"generic": name}` node bound in `bindings` replaced.
fn substitute(ty: &Value, bindings: &BTreeMap<&str, Value>) -> Value {
    match ty {
        Value::Object(map) => {
            if let Some(bound) = map
                .get("generic")
                .and_then(Value::as_str)
                .and_then(|name| bindings.get(name))
            {
                return bound.clone();
            }
            Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), substitute(v, bindings)))
                    .collect(),
            )
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| substitute(v, bindings)).collect())
        }
        other => other.clone(),
    }
}

/// Every item reachable from the crate root through public modules and
/// `pub use` re-exports, keyed by id, each at the shortest such path.
fn public_paths(doc: &Value, index: &Map<String, Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(root) = id_key(&doc["root"]) else {
        return out;
    };
    let Some(name) = index.get(&root).and_then(|r| r["name"].as_str()) else {
        return out;
    };
    let mut queue = VecDeque::from([(root, name.to_string())]);
    let mut seen = BTreeSet::new();
    while let Some((module, prefix)) = queue.pop_front() {
        if !seen.insert(module.clone()) {
            continue;
        }
        let Some(items) = index
            .get(&module)
            .and_then(|m| m["inner"]["module"]["items"].as_array())
        else {
            continue;
        };
        for id in items.iter().filter_map(id_key) {
            let Some(item) = index.get(&id) else {
                continue;
            };
            if item["visibility"].as_str() != Some("public") {
                continue;
            }
            let (target, name, glob) = match item["inner"].get("use") {
                Some(re) => match (id_key(&re["id"]), re["name"].as_str()) {
                    (Some(target), Some(name)) => (
                        target,
                        name.to_string(),
                        re["is_glob"].as_bool().unwrap_or(false),
                    ),
                    _ => continue,
                },
                None => match item["name"].as_str() {
                    Some(name) => (id.clone(), name.to_string(), false),
                    None => continue,
                },
            };
            let is_module = index
                .get(&target)
                .is_some_and(|t| t["inner"].get("module").is_some());
            let path = match glob {
                true => prefix.clone(),
                false => format!("{prefix}::{name}"),
            };
            if is_module {
                queue.push_back((target, path));
            } else {
                out.entry(target).or_insert(path);
            }
        }
    }
    out
}

/// Rustdoc writes an id as a number in a type node and as a string key in
/// `index`, so every lookup goes through one spelling.
fn id_key(id: &Value) -> Option<String> {
    match id {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn join_path(segments: &[Value]) -> String {
    segments
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("::")
}

/// A best-effort spelling of a node the resolver has no arm for, so the gap
/// it reports still names something the reader can find in the source.
fn render(ty: &Value) -> String {
    ty.as_object()
        .and_then(|o| o.keys().next().cloned())
        .unwrap_or_else(|| "?".into())
}
