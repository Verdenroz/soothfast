#![recursion_limit = "512"]
//! Rust API idioms a real crate leans on, walked over a synthetic document:
//! a crate-wide `Result<T>` alias, `impl Into<String>` parameters, private
//! fields of unbindable types, and an `async fn new`.

mod fixture;

use fixture::{
    auto_impl, borrowed, enum_item, field, func, method, opts, path, prim, record, struct_item,
    variant,
};
use serde_json::{Value, json};
use soothfast_bind::foreign::TypeTable;
use soothfast_bind::gap::Gap;
use soothfast_bind::model::{Surface, Ty, VariantFields};
use soothfast_bind::plan::{VariantShape, lower};
use soothfast_bind::walk::surface;
use soothfast_bind::{BindKind, BindOptions};

fn alias(name: &str, target: Value) -> Value {
    json!({
        "name": name, "docs": Value::Null, "attrs": [], "visibility": "public",
        "inner": { "type_alias": {
            "type": target,
            "generics": { "params": [
                { "name": "T", "kind": { "type": { "bounds": [], "default": Value::Null, "is_synthetic": false } } }
            ], "where_predicates": [] } } },
    })
}

fn into_string() -> Value {
    json!({ "impl_trait": [ { "trait_bound": {
        "trait": { "path": "Into", "id": 61, "args": { "angle_bracketed": {
            "args": [ { "type": path("String", 92, &[]) } ], "constraints": [] } } },
        "generic_params": [], "modifier": "none" } } ] })
}

fn with_synthetic_param(mut f: Value) -> Value {
    f["inner"]["function"]["generics"]["params"] = json!([
        { "name": "impl Into<String>", "kind": { "type": {
            "bounds": [], "default": Value::Null, "is_synthetic": true } } }
    ]);
    f
}

fn non_exhaustive(mut item: Value) -> Value {
    item["attrs"] = json!(["#[non_exhaustive]"]);
    item
}

fn module(name: &str, public: bool, items: &[u64]) -> Value {
    json!({
        "name": name, "docs": Value::Null, "attrs": [],
        "visibility": if public { "public" } else { "crate" },
        "inner": { "module": { "is_crate": name == "acme", "items": items, "is_stripped": false } },
    })
}

fn reexport(name: &str, target: u64) -> Value {
    json!({
        "name": name, "docs": Value::Null, "attrs": [], "visibility": "public",
        "inner": { "use": { "source": format!("core::{name}"), "name": name, "id": target, "is_glob": false } },
    })
}

/// `Client` and `open` live in a private `core` module and reach the crate
/// root through `pub use`; `read` is only reachable through a glob.
fn doc() -> Value {
    let std_result = |ok: Value, err: Value| path("std::result::Result", 67, &[ok, err]);
    json!({
        "root": 0,
        "index": {
            "0": module("acme", true, &[40, 41, 42, 43, 45, 46]),
            "40": module("core", false, &[1, 2, 3, 5, 6, 30, 32]),
            "41": reexport("Client", 3),
            "45": reexport("Level", 5),
            "46": reexport("Bag", 6),
            "42": reexport("open", 30),
            "43": json!({ "name": Value::Null, "docs": Value::Null, "attrs": [], "visibility": "public",
                "inner": { "use": { "source": "util::*", "name": "util", "id": 44, "is_glob": true } } }),
            "44": module("util", false, &[31]),
            "1": alias("Result", std_result(json!({ "generic": "T" }), path("String", 92, &[]))),
            "2": alias("Outcome", path("Result", 1, &[json!({ "generic": "T" })])),
            "3": struct_item("Client", &[10, 11, 15, 16], &[4, 47]),
            "47": auto_impl("Clone", false),
            "4": json!({ "name": Value::Null, "docs": Value::Null, "attrs": [],
                "inner": { "impl": { "trait": Value::Null, "items": [20, 21, 22, 23] } } }),
            "10": field("symbol", path("String", 92, &[]), true),
            "11": field("clock", path("DateTime", 99, &[]), false),
            "15": field("price", prim("f64"), true),
            "16": field("maybe", path("Option", 93, &[prim("i64")]), true),
            "20": func("new", &[("symbol", into_string())], path("Result", 1, &[json!({ "generic": "Self" })]), true),
            "21": func("chart", &[("self", borrowed(json!({ "generic": "Self" }), false))], path("Result", 1, &[prim("i64")]), true),
            "22": func("nested", &[("self", borrowed(json!({ "generic": "Self" }), false))], path("Outcome", 2, &[prim("u32")]), false),
            "23": func("symbol", &[("self", borrowed(json!({ "generic": "Self" }), false))], borrowed(prim("str"), false), false),
            "5": enum_item("Level", &[60, 61], &[]),
            "60": variant("Low", json!("plain")),
            "61": variant("High", json!("plain")),
            "6": struct_item("Bag", &[12, 13, 14], &[]),
            "14": field("owner", path("Client", 3, &[]), true),
            "12": field("items", path("Vec", 90, &[path("Client", 3, &[])]), true),
            "13": field("level", path("Option", 93, &[path("Level", 5, &[])]), true),
            "30": with_synthetic_param(func("open", &[("name", into_string())], prim("bool"), false)),
            "32": func("probe", &[], std_result(prim("bool"), path("Failure", 7, &[])), false),
            "7": non_exhaustive(enum_item("Failure", &[70, 71, 72], &[])),
            "70": variant("NotFound", json!({ "struct": { "fields": [73, 74, 75], "has_stripped_fields": false } })),
            "73": field("symbol", path("String", 92, &[]), true),
            "74": field("retry", path("Option", 93, &[prim("i64")]), true),
            "75": field("client", path("Client", 3, &[]), true),
            "71": variant("Wrapped", json!({ "tuple": [76] })),
            "76": field("0", prim("i64"), true),
            "72": variant("Busy", json!("plain")),
            "31": func("read", &[("name", json!({ "impl_trait": [ { "trait_bound": {
                "trait": { "path": "AsRef", "id": 62, "args": { "angle_bracketed": {
                    "args": [ { "type": prim("str") } ], "constraints": [] } } },
                "generic_params": [], "modifier": "none" } } ] }))], prim("bool"), false),
        },
        "paths": {
            "1": { "crate_id": 0, "path": ["acme", "core", "Result"], "kind": "type_alias" },
            "2": { "crate_id": 0, "path": ["acme", "core", "Outcome"], "kind": "type_alias" },
            "3": { "crate_id": 0, "path": ["acme", "core", "Client"], "kind": "struct" },
            "5": { "crate_id": 0, "path": ["acme", "core", "Level"], "kind": "enum" },
            "6": { "crate_id": 0, "path": ["acme", "core", "Bag"], "kind": "struct" },
            "90": { "crate_id": 1, "path": ["alloc", "vec", "Vec"], "kind": "struct" },
            "93": { "crate_id": 1, "path": ["core", "option", "Option"], "kind": "enum" },
            "30": { "crate_id": 0, "path": ["acme", "core", "open"], "kind": "function" },
            "32": { "crate_id": 0, "path": ["acme", "core", "probe"], "kind": "function" },
            "7": { "crate_id": 0, "path": ["acme", "core", "Failure"], "kind": "enum" },
            "31": { "crate_id": 0, "path": ["acme", "util", "read"], "kind": "function" },
            "67": { "crate_id": 1, "path": ["core", "result", "Result"], "kind": "enum" },
            "92": { "crate_id": 1, "path": ["alloc", "string", "String"], "kind": "struct" },
            "99": { "crate_id": 2, "path": ["chrono", "DateTime"], "kind": "struct" },
        }
    })
}

/// Rustdoc stamps every index entry with its own id; the fixture builders
/// leave it to the caller.
fn stamped(mut doc: Value) -> Value {
    let index = doc["index"].as_object_mut().expect("index");
    for (key, item) in index.iter_mut() {
        item["id"] = json!(key.parse::<u64>().expect("numeric id"));
    }
    doc
}

fn walk() -> (Surface, Vec<Gap>) {
    let records = vec![
        record("acme::core::Client", "struct"),
        record("acme::core::Level", "enum"),
        record("acme::core::Bag", "struct"),
        method("acme::core::Client::symbol", "Client"),
        record("acme::core::open", "fn"),
        record("acme::core::probe", "fn"),
        record("acme::util::read", "fn"),
        method("acme::core::Client::new", "Client"),
        method("acme::core::Client::chart", "Client"),
        method("acme::core::Client::nested", "Client"),
    ];
    surface(&stamped(doc()), &TypeTable::with_defaults(), &records).expect("walks")
}

#[test]
fn a_thrown_enum_error_is_read_once_with_its_variants_whether_exported_or_not() {
    let (surface, _) = walk();
    assert_eq!(
        find(&surface, "acme::core::probe").throws,
        Some(Ty::Opaque("acme::core::Failure".into()))
    );
    assert_eq!(surface.errors.len(), 1, "{:?}", surface.errors);
    let failure = &surface.errors[0];
    assert_eq!(failure.name, "Failure");
    assert_eq!(failure.rust_path, "acme::core::Failure");
    assert!(failure.non_exhaustive);
    let variants = failure.variants.as_ref().expect("an enum");
    let names: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["NotFound", "Wrapped", "Busy"]);
    let VariantFields::Named(fields) = &variants[0].fields else {
        panic!("{:?}", variants[0]);
    };
    let tys: Vec<(&str, &Ty)> = fields.iter().map(|f| (f.name.as_str(), &f.ty)).collect();
    assert_eq!(
        tys,
        [
            ("symbol", &Ty::Str),
            ("retry", &Ty::Optional(Box::new(Ty::I64))),
            ("client", &Ty::Class("Client".into())),
        ]
    );
    assert_eq!(variants[1].fields, VariantFields::Tuple(vec![Ty::I64]));
    assert_eq!(variants[2].fields, VariantFields::Unit);
}

#[test]
fn a_thrown_enum_error_lowers_to_a_class_per_variant_keeping_only_plain_fields() {
    let (surface, gaps) = walk();
    let plan = lower(&surface, gaps, &opts(), BindKind::Python).expect("lowers");
    assert_eq!(plan.errors.len(), 1, "{:?}", plan.errors);
    let failure = &plan.errors[0];
    assert_eq!(failure.rust_path, "acme::core::Failure");
    let shapes: Vec<(&str, VariantShape)> = failure
        .variants
        .iter()
        .map(|v| (v.name.as_str(), v.shape))
        .collect();
    assert_eq!(
        shapes,
        [
            ("NotFound", VariantShape::Named),
            ("Wrapped", VariantShape::Tuple),
            ("Busy", VariantShape::Unit),
        ]
    );
    assert_eq!(
        failure.variants[0].fields,
        [
            ("symbol".to_string(), Ty::Str),
            ("retry".to_string(), Ty::Optional(Box::new(Ty::I64))),
        ]
    );
    assert!(failure.variants[1].fields.is_empty());
}

#[test]
fn the_python_glue_raises_a_package_hierarchy_with_variant_fields_as_attributes() {
    let glue = python_glue();
    assert!(glue.contains(
        "::pyo3::create_exception!(acme_core, Error, ::pyo3::exceptions::PyException, \"Base of every error acme-core raises.\");"
    ));
    assert!(glue.contains(
        "::pyo3::create_exception!(acme_core, Failure, Error, \"A `acme::core::Failure` error.\");"
    ));
    assert!(glue.contains(
        "::pyo3::create_exception!(acme_core, NotFound, Failure, \"`Failure::NotFound`.\");"
    ));
    assert!(glue.contains(
        "::pyo3::create_exception!(acme_core, Wrapped, Failure, \"`Failure::Wrapped`.\");"
    ));
    assert!(
        glue.contains("::pyo3::create_exception!(acme_core, Busy, Failure, \"`Failure::Busy`.\");")
    );
    assert!(glue.contains("struct BindErroracmecoreFailure(::acme::core::Failure);"));
    assert!(
        glue.contains("#[allow(unreachable_patterns)]\n    fn from(err: BindErroracmecoreFailure)")
    );
    assert!(glue.contains(
        "            ::acme::core::Failure::NotFound { symbol, retry, .. } => {\n                let e = NotFound::new_err(message);\n                ::pyo3::Python::attach(|py| {\n                    let value = e.value(py);\n                    let _ = value.setattr(\"symbol\", symbol);\n                    let _ = value.setattr(\"retry\", retry);\n                });\n                e\n            }"
    ));
    assert!(!glue.contains("setattr(\"client\""), "{glue}");
    assert!(glue.contains("::acme::core::Failure::Wrapped(..) => Wrapped::new_err(message),"));
    assert!(glue.contains("::acme::core::Failure::Busy => Busy::new_err(message),"));
    assert!(glue.contains("            _ => Failure::new_err(message),"));
    assert!(glue.contains("fn from(err: BindErrorString) -> ::pyo3::PyErr {\n        let message = ::std::string::ToString::to_string(&err.0);\n        Error::new_err(message)"));
    assert!(!glue.contains("PyRuntimeError"), "{glue}");
    let module = &glue[glue.find("#[pymodule").expect("module block")..];
    for name in ["Error", "Failure", "NotFound", "Wrapped", "Busy"] {
        assert!(
            module.contains(&format!("m.add(\"{name}\", m.py().get_type::<{name}>())?;")),
            "{module}"
        );
    }
    assert!(module.find("add_class::<Bag>").unwrap() < module.find("m.add(\"Error\"").unwrap());
}

/// The document plus `stamp(at: DateTime) -> DateTime`,
/// `maybe_stamp() -> Option<DateTime>` and a public `Client.clock`, which
/// only a `[bind.types]` mapping makes bindable.
fn doc_with_stamp() -> Value {
    let mut doc = doc();
    doc["index"]["33"] = func(
        "stamp",
        &[("at", path("DateTime", 99, &[]))],
        path("DateTime", 99, &[]),
        false,
    );
    doc["index"]["34"] = func(
        "maybe_stamp",
        &[],
        path("Option", 93, &[path("DateTime", 99, &[])]),
        false,
    );
    doc["index"]["11"] = field("clock", path("DateTime", 99, &[]), true);
    doc["index"]["40"]["inner"]["module"]["items"]
        .as_array_mut()
        .expect("items")
        .extend([json!(33), json!(34)]);
    doc["paths"]["33"] =
        json!({ "crate_id": 0, "path": ["acme", "core", "stamp"], "kind": "function" });
    doc["paths"]["34"] =
        json!({ "crate_id": 0, "path": ["acme", "core", "maybe_stamp"], "kind": "function" });
    doc
}

#[test]
fn the_python_glue_parses_a_mapped_type_in_and_renders_it_out() {
    let mut table = TypeTable::with_defaults();
    table.insert("chrono::DateTime", Ty::Text("chrono::DateTime".into()));
    let records = vec![
        record("acme::core::Client", "struct"),
        method("acme::core::Client::new", "Client"),
        record("acme::core::stamp", "fn"),
        record("acme::core::maybe_stamp", "fn"),
    ];
    let (walked, gaps) = surface(&stamped(doc_with_stamp()), &table, &records).expect("walks");
    let files = BindKind::Python
        .emit(&walked, gaps, &opts())
        .expect("emits");
    let glue = &files.files["src/lib.rs"];
    let parse = "parse().map_err(|e| ::pyo3::exceptions::PyValueError::new_err(::std::string::ToString::to_string(&e)))?";
    assert!(
        glue.contains(&format!(
            "fn stamp(at: String) -> PyResult<String> {{\n    let at = at.{parse};\n    Ok(::std::string::ToString::to_string(&::acme::core::stamp(at)))\n}}"
        )),
        "{glue}"
    );
    assert!(
        glue.contains(
            "fn maybe_stamp() -> Option<String> {\n    ::acme::core::maybe_stamp().map(|v| ::std::string::ToString::to_string(&v))\n}"
        ),
        "{glue}"
    );
    assert!(
        glue.contains(
            "fn clock(&self) -> String {\n        ::std::string::ToString::to_string(&self.0.clock)\n    }"
        ),
        "{glue}"
    );
    assert!(
        glue.contains(&format!(
            "fn set_clock(&mut self, value: String) -> PyResult<()> {{\n        self.0.clock = value.{parse};\n        Ok(())\n    }}"
        )),
        "{glue}"
    );
}

#[test]
fn a_mapped_foreign_type_crosses_as_text_into_python_and_is_reported_elsewhere() {
    let mut table = TypeTable::with_defaults();
    table.insert("chrono::DateTime", Ty::Text("chrono::DateTime".into()));
    let records = vec![record("acme::core::stamp", "fn")];
    let (walked, gaps) = surface(&stamped(doc_with_stamp()), &table, &records).expect("walks");
    let stamp = find(&walked, "acme::core::stamp");
    assert_eq!(stamp.params[0].ty, Ty::Text("chrono::DateTime".into()));
    assert_eq!(stamp.ret, Ty::Text("chrono::DateTime".into()));
    assert!(gaps.is_empty(), "{gaps:?}");

    let python = lower(&walked, gaps.clone(), &opts(), BindKind::Python).expect("lowers");
    assert!(python.functions.iter().any(|f| f.name == "stamp"));
    assert!(python.gaps.is_empty(), "{:?}", python.gaps);
    let go = lower(&walked, gaps, &opts(), BindKind::Go).expect("lowers");
    assert!(go.functions.is_empty());
    let why = go
        .gaps
        .iter()
        .map(|g| g.explain())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(why.contains("crosses into Python only for now"), "{why}");

    let (_, unmapped) = surface(
        &stamped(doc_with_stamp()),
        &TypeTable::with_defaults(),
        &records,
    )
    .expect("walks");
    let why = unmapped
        .iter()
        .map(|g| g.explain())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        why.contains("`\"chrono::DateTime\" = \"str\"` crosses it as a string"),
        "{why}"
    );
}

#[test]
fn a_blocking_twin_holds_the_lock_when_the_owner_is_not_sync_and_a_static_releases_it() {
    let glue = python_glue();
    assert!(
        glue.contains(
            "    /// Blocking form of `chart`: runs the call to completion on the package's runtime.\n    fn chart_blocking(&self) -> PyResult<i64> {\n        Ok(runtime().block_on(self.0.chart()).map_err(BindErrorString)?)\n    }"
        ),
        "{glue}"
    );
    assert!(
        glue.contains(
            "    #[staticmethod]\n    fn new_blocking(py: Python<'_>, symbol: String) -> PyResult<Client> {\n        let out = py.detach(|| runtime().block_on(::acme::Client::new(symbol)));\n        Ok(Client(out.map_err(BindErrorString)?))\n    }"
        ),
        "{glue}"
    );
    assert!(glue.contains("async fn chart(&self)"), "{glue}");
}

/// The document with `Client` marked `Sync` and an async free `poll`.
fn doc_with_sync_client_and_poll() -> Value {
    let mut doc = doc();
    doc["index"]["3"]["inner"]["struct"]["impls"]
        .as_array_mut()
        .expect("impls")
        .extend([json!(77), json!(78)]);
    doc["index"]["77"] = auto_impl("Sync", false);
    doc["index"]["78"] = auto_impl("Send", false);
    doc["index"]["35"] = func("poll", &[], prim("bool"), true);
    doc["index"]["40"]["inner"]["module"]["items"]
        .as_array_mut()
        .expect("items")
        .push(json!(35));
    doc["paths"]["35"] =
        json!({ "crate_id": 0, "path": ["acme", "core", "poll"], "kind": "function" });
    doc
}

#[test]
fn a_blocking_twin_releases_the_lock_for_a_sync_owner_and_a_free_twin_is_registered() {
    let records = vec![
        record("acme::core::Client", "struct"),
        method("acme::core::Client::chart", "Client"),
        record("acme::core::poll", "fn"),
    ];
    let (walked, gaps) = surface(
        &stamped(doc_with_sync_client_and_poll()),
        &TypeTable::with_defaults(),
        &records,
    )
    .expect("walks");
    let files = BindKind::Python
        .emit(&walked, gaps, &opts())
        .expect("emits");
    let glue = &files.files["src/lib.rs"];
    assert!(
        glue.contains(
            "    fn chart_blocking(&self, py: Python<'_>) -> PyResult<i64> {\n        let out = py.detach(|| runtime().block_on(self.0.chart()));\n        Ok(out.map_err(BindErrorString)?)\n    }"
        ),
        "{glue}"
    );
    assert!(
        glue.contains(
            "#[pyfunction]\nfn poll_blocking(py: Python<'_>) -> bool {\n    py.detach(|| runtime().block_on(::acme::core::poll()))\n}"
        ),
        "{glue}"
    );
    assert!(
        glue.contains("m.add_function(wrap_pyfunction!(poll_blocking, m)?)?;"),
        "{glue}"
    );
    let unconfigured = BindOptions {
        blocking: false,
        ..opts()
    };
    let files = BindKind::Python
        .emit(&walked, Vec::new(), &unconfigured)
        .expect("emits");
    assert!(!files.files["src/lib.rs"].contains("_blocking"));
}

#[test]
fn a_blocking_twin_whose_name_is_taken_is_an_error() {
    let mut doc = doc_with_sync_client_and_poll();
    doc["index"]["36"] = func("poll_blocking", &[], prim("bool"), false);
    doc["index"]["40"]["inner"]["module"]["items"]
        .as_array_mut()
        .expect("items")
        .push(json!(36));
    doc["paths"]["36"] =
        json!({ "crate_id": 0, "path": ["acme", "core", "poll_blocking"], "kind": "function" });
    let records = vec![
        record("acme::core::poll", "fn"),
        record("acme::core::poll_blocking", "fn"),
    ];
    let (walked, gaps) =
        surface(&stamped(doc), &TypeTable::with_defaults(), &records).expect("walks");
    let err = BindKind::Python
        .emit(&walked, gaps, &opts())
        .expect_err("collides");
    assert!(err.contains("`poll_blocking`"), "{err}");
}

fn find<'a>(surface: &'a Surface, id: &str) -> &'a soothfast_bind::model::ExportedFn {
    surface
        .fns
        .iter()
        .find(|f| f.id == id)
        .unwrap_or_else(|| panic!("{id} not walked"))
}

#[test]
fn a_single_argument_result_alias_expands_to_the_result_it_names() {
    let (surface, gaps) = walk();
    let chart = find(&surface, "acme::core::Client::chart");
    assert_eq!(chart.ret, Ty::I64);
    assert_eq!(chart.throws, Some(Ty::Str));
    assert!(
        !gaps.iter().any(|g| g.at() == "acme::core::Client::chart"),
        "{gaps:?}"
    );
}

#[test]
fn an_alias_of_an_alias_expands_all_the_way() {
    let (surface, _) = walk();
    let nested = find(&surface, "acme::core::Client::nested");
    assert_eq!(nested.ret, Ty::U32);
    assert_eq!(nested.throws, Some(Ty::Str));
}

#[test]
fn an_impl_into_parameter_binds_as_the_type_it_converts_from() {
    let (surface, gaps) = walk();
    assert_eq!(find(&surface, "acme::core::open").params[0].ty, Ty::Str);
    assert_eq!(find(&surface, "acme::util::read").params[0].ty, Ty::Str);
    assert!(
        !gaps
            .iter()
            .any(|g| matches!(g, Gap::Generic { .. } | Gap::Erased { .. })),
        "{gaps:?}"
    );
}

#[test]
fn a_private_field_of_a_foreign_type_is_not_a_gap() {
    let (_, gaps) = walk();
    assert!(
        !gaps.iter().any(|g| g.at() == "acme::core::Client.clock"),
        "{gaps:?}"
    );
}

#[test]
fn an_async_new_is_a_static_factory_rather_than_a_constructor() {
    let (surface, gaps) = walk();
    let new = find(&surface, "acme::core::Client::new");
    assert_eq!(new.ret, Ty::Class("Client".into()));
    assert!(new.is_async);
    let plan = lower(&surface, gaps, &opts(), BindKind::Python).expect("lowers");
    let client = plan
        .classes
        .iter()
        .find(|c| c.name == "Client")
        .expect("Client is a class");
    assert!(client.ctor.is_none());
    assert!(client.statics.iter().any(|f| f.name == "new" && f.is_async));
}

#[test]
fn an_item_defined_in_a_private_module_is_spelled_by_its_public_reexport() {
    let (surface, _) = walk();
    let client = surface
        .types
        .iter()
        .find(|t| t.name == "Client")
        .expect("Client walked");
    assert_eq!(client.id, "acme::core::Client");
    assert_eq!(client.rust_path, "acme::Client");
    assert_eq!(
        find(&surface, "acme::core::Client::new").rust_path,
        "acme::Client::new"
    );
    assert_eq!(find(&surface, "acme::core::open").rust_path, "acme::open");
    assert_eq!(find(&surface, "acme::util::read").rust_path, "acme::read");
}

fn python_glue() -> String {
    let (surface, gaps) = walk();
    let files = BindKind::Python
        .emit(&surface, gaps, &opts())
        .expect("emits");
    files.files["src/lib.rs"].clone()
}

#[test]
fn a_borrowed_str_return_is_owned_by_the_python_glue_and_reported_elsewhere() {
    let (surface, gaps) = walk();
    assert!(find(&surface, "acme::core::Client::symbol").ret_borrowed);
    assert!(python_glue().contains("self.0.symbol().to_owned()"));
    let go = lower(&surface, gaps, &opts(), BindKind::Go).expect("lowers");
    assert!(
        go.gaps
            .iter()
            .any(|g| g.at() == "acme::core::Client::symbol"),
        "{:?}",
        go.gaps
    );
}

#[test]
fn a_handle_list_field_reads_but_never_writes_and_an_optional_enum_field_converts() {
    let glue = python_glue();
    assert!(
        glue.contains(
            "fn items(&self) -> ClientSeq {\n        ClientSeq::new(self.0.items.clone())"
        )
    );
    assert!(!glue.contains("fn set_items"), "{glue}");
    assert!(glue.contains("fn set_level(&mut self, value: Option<Level>)"));
    assert!(glue.contains("self.0.level = value.map(::std::convert::Into::into);"));
}

#[test]
fn a_handle_list_field_gets_one_seq_class_with_on_demand_handles_and_columns() {
    let glue = python_glue();
    assert!(
        glue.contains(
            "#[pyclass(name = \"ClientSeq\")]\npub struct ClientSeq(Vec<::acme::Client>);"
        )
    );
    assert_eq!(glue.matches("pub struct ClientSeq(").count(), 1);
    assert!(glue.contains("fn __getitem__(&self, index: isize) -> ::pyo3::PyResult<Client> {"));
    assert!(glue.contains(
        "fn tolist(&self) -> Vec<Client> {\n        self.0.iter().cloned().map(Client).collect()"
    ));
    assert!(glue.contains("format!(\"ClientSeq(len={})\", self.0.len())"));
    assert!(glue.contains("fn symbol(&self) -> Vec<String> {\n        self.0.iter().map(|v| v.symbol.clone()).collect()"));
    assert!(glue.contains("fn price(&self) -> F64Array {\n        F64Array::new(self.0.iter().map(|v| v.price).collect())"));
    assert!(glue.contains(
        "fn maybe(&self) -> Vec<Option<i64>> {\n        self.0.iter().map(|v| v.maybe).collect()"
    ));
    assert_eq!(glue.matches("pub struct F64Array(").count(), 1);
    assert!(glue.contains("m.add_class::<F64Array>()?;"));
    let seq_at = glue
        .find("m.add_class::<ClientSeq>()?;")
        .expect("registers the seq");
    let class_at = glue
        .find("m.add_class::<Client>()?;")
        .expect("registers the class");
    assert!(seq_at < class_at, "{glue}");
    let seq_def = glue.find("pub struct ClientSeq(").expect("defines the seq");
    let class_def = glue.find("pub struct Client(").expect("defines the class");
    assert!(seq_def < class_def, "{glue}");
}

#[test]
fn a_field_holding_a_clone_exported_type_reads_as_a_fresh_handle_in_python_only() {
    let (surface, gaps) = walk();
    let python = lower(&surface, gaps.clone(), &opts(), BindKind::Python).expect("lowers");
    let bag = python
        .classes
        .iter()
        .find(|c| c.name == "Bag")
        .expect("Bag");
    let fields: Vec<&str> = bag.accessors.iter().map(|a| a.field.as_str()).collect();
    assert_eq!(fields, ["items", "level", "owner"]);
    assert!(
        python_glue().contains("fn owner(&self) -> Client {\n        Client(self.0.owner.clone())")
    );
    let go = lower(&surface, gaps, &opts(), BindKind::Go).expect("lowers");
    assert!(
        go.gaps.iter().any(|g| g.at() == "Bag.owner"),
        "{:?}",
        go.gaps
    );
}
