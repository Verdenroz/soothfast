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
use soothfast_bind::BindKind;
use soothfast_bind::foreign::TypeTable;
use soothfast_bind::gap::Gap;
use soothfast_bind::model::{Surface, Ty};
use soothfast_bind::plan::lower;
use soothfast_bind::walk::surface;

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
            "40": module("core", false, &[1, 2, 3, 5, 6, 30]),
            "41": reexport("Client", 3),
            "45": reexport("Level", 5),
            "46": reexport("Bag", 6),
            "42": reexport("open", 30),
            "43": json!({ "name": Value::Null, "docs": Value::Null, "attrs": [], "visibility": "public",
                "inner": { "use": { "source": "util::*", "name": "util", "id": 44, "is_glob": true } } }),
            "44": module("util", false, &[31]),
            "1": alias("Result", std_result(json!({ "generic": "T" }), path("String", 92, &[]))),
            "2": alias("Outcome", path("Result", 1, &[json!({ "generic": "T" })])),
            "3": struct_item("Client", &[10, 11], &[4, 47]),
            "47": auto_impl("Clone", false),
            "4": json!({ "name": Value::Null, "docs": Value::Null, "attrs": [],
                "inner": { "impl": { "trait": Value::Null, "items": [20, 21, 22, 23] } } }),
            "10": field("symbol", path("String", 92, &[]), true),
            "11": field("clock", path("DateTime", 99, &[]), false),
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
        record("acme::util::read", "fn"),
        method("acme::core::Client::new", "Client"),
        method("acme::core::Client::chart", "Client"),
        method("acme::core::Client::nested", "Client"),
    ];
    surface(&stamped(doc()), &TypeTable::with_defaults(), &records).expect("walks")
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
