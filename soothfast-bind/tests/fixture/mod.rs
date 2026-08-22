//! One synthetic rustdoc document, shared by every test in this crate.
//!
//! Shapes are pinned against a live nightly probe; synthetic input keeps the
//! suite off nightly and stable across rustdoc releases.

// Each test binary compiles this module separately and uses part of it.
#![allow(dead_code)]

use serde_json::{Map, Value, json};

use soothfast_bind::foreign::TypeTable;
use soothfast_bind::gap::Gap;
use soothfast_bind::model::{ExportRecord, Surface};
use soothfast_bind::plan::{BindingPlan, lower};
use soothfast_bind::walk::surface;
use soothfast_bind::{BindKind, BindOptions};

pub fn prim(p: &str) -> Value {
    json!({ "primitive": p })
}

pub fn path(name: &str, id: u64, args: &[Value]) -> Value {
    let args = if args.is_empty() {
        Value::Null
    } else {
        json!({ "angle_bracketed": {
            "args": args.iter().map(|a| json!({ "type": a })).collect::<Vec<_>>(),
            "constraints": [] } })
    };
    json!({ "resolved_path": { "path": name, "id": id, "args": args } })
}

pub fn borrowed(ty: Value, mutable: bool) -> Value {
    json!({ "borrowed_ref": { "lifetime": null, "is_mutable": mutable, "type": ty } })
}

pub fn func(name: &str, inputs: &[(&str, Value)], output: Value, is_async: bool) -> Value {
    let inputs: Vec<Value> = inputs.iter().map(|(n, t)| json!([n, t])).collect();
    json!({
        "name": name, "docs": Value::Null, "attrs": [], "visibility": "public",
        "inner": { "function": {
            "sig": { "inputs": inputs, "output": output },
            "header": { "is_async": is_async },
            "generics": { "params": [], "where_predicates": [] } } },
    })
}

pub fn field(name: &str, ty: Value, public: bool) -> Value {
    json!({
        "name": name, "docs": Value::Null, "attrs": [],
        "visibility": if public { "public" } else { "default" },
        "inner": { "struct_field": ty },
    })
}

pub fn struct_item(name: &str, fields: &[u64], impls: &[u64]) -> Value {
    json!({
        "name": name, "docs": Value::Null, "attrs": [], "visibility": "public",
        "inner": { "struct": {
            "kind": { "plain": { "fields": fields, "has_stripped_fields": false } },
            "generics": { "params": [], "where_predicates": [] },
            "impls": impls } },
    })
}

/// A synthetic auto-trait impl, as rustdoc records one.
pub fn auto_impl(name: &str, negative: bool) -> Value {
    json!({
        "name": Value::Null, "docs": Value::Null, "attrs": [],
        "inner": { "impl": {
            "trait": { "path": name, "id": 99, "args": Value::Null },
            "is_negative": negative, "is_synthetic": true, "items": [] } },
    })
}

pub fn enum_item(name: &str, variants: &[u64]) -> Value {
    json!({
        "name": name, "docs": Value::Null, "attrs": [], "visibility": "public",
        "inner": { "enum": {
            "variants": variants,
            "generics": { "params": [], "where_predicates": [] },
            "impls": [] } },
    })
}

pub fn variant(name: &str, kind: Value) -> Value {
    json!({
        "name": name, "docs": Value::Null, "attrs": [],
        "inner": { "variant": { "kind": kind } },
    })
}

/// `acme`, with a free fn, a struct and its inherent impl, and an enum.
pub fn doc() -> Value {
    let mut index = Map::new();
    let mut insert = |id: u64, item: Value| {
        index.insert(id.to_string(), item);
    };

    insert(
        1,
        func(
            "normalize",
            &[
                ("input", path("Vec", 90, &[prim("f64")])),
                ("factor", prim("f64")),
            ],
            path("Vec", 90, &[prim("f64")]),
            false,
        ),
    );
    insert(
        5,
        func(
            "with_time",
            &[("at", path("DateTime", 10, &[]))],
            prim("u32"),
            false,
        ),
    );
    insert(
        6,
        func(
            "digest",
            &[("data", borrowed(json!({ "slice": prim("u8") }), false))],
            path("Vec", 90, &[prim("u8")]),
            false,
        ),
    );
    insert(
        7,
        func(
            "index_all",
            &[],
            path("HashMap", 91, &[path("String", 92, &[]), prim("u32")]),
            false,
        ),
    );

    insert(
        8,
        func(
            "merge",
            &[("base", path("Counter", 2, &[]))],
            prim("i64"),
            false,
        ),
    );
    // Parameter names that collide with what the C backend generates around
    // them: the receiver binding, the error out-parameter, and a C keyword.
    insert(
        11,
        func(
            "stamp",
            &[
                ("handle", prim("i64")),
                ("error", prim("f64")),
                ("register", borrowed(json!({ "slice": prim("u8") }), false)),
            ],
            prim("u64"),
            false,
        ),
    );
    insert(
        9,
        func(
            "trim",
            &[("input", borrowed(json!({ "slice": prim("f64") }), false))],
            path("Option", 93, &[path("Vec", 90, &[prim("f64")])]),
            false,
        ),
    );

    insert(2, struct_item("Counter", &[20, 21], &[30, 37, 38]));
    insert(37, auto_impl("Send", false));
    insert(38, auto_impl("Sync", false));
    insert(20, field("value", prim("i64"), true));
    insert(21, field("label", path("String", 92, &[]), false));

    insert(
        30,
        json!({ "name": Value::Null, "docs": Value::Null, "attrs": [],
                "inner": { "impl": { "trait": Value::Null, "items": [31, 32, 33, 34, 35, 36] } } }),
    );
    insert(
        31,
        func(
            "new",
            &[("start", prim("i64"))],
            json!({ "generic": "Self" }),
            false,
        ),
    );
    insert(
        32,
        func(
            "bump",
            &[
                ("self", borrowed(json!({ "generic": "Self" }), false)),
                ("by", prim("i64")),
            ],
            path("Result", 93, &[prim("i64"), path("String", 92, &[])]),
            false,
        ),
    );
    insert(
        33,
        func(
            "consume",
            &[("self", json!({ "generic": "Self" }))],
            prim("i64"),
            false,
        ),
    );
    insert(
        34,
        func(
            "refresh",
            &[("self", borrowed(json!({ "generic": "Self" }), false))],
            prim("u32"),
            true,
        ),
    );

    insert(
        35,
        func(
            "bump_all",
            &[
                ("self", borrowed(json!({ "generic": "Self" }), false)),
                ("by", path("Vec", 90, &[prim("i64")])),
            ],
            prim("i64"),
            false,
        ),
    );

    insert(
        36,
        func(
            "at",
            &[
                ("self", borrowed(json!({ "generic": "Self" }), false)),
                ("level", path("Level", 4, &[])),
            ],
            prim("i64"),
            false,
        ),
    );

    insert(4, enum_item("Level", &[45, 46]));
    insert(45, variant("Low", json!("plain")));
    insert(46, variant("High", json!("plain")));

    insert(3, enum_item("Mode", &[40, 41, 42]));
    insert(40, variant("Fast", json!("plain")));
    insert(41, variant("Precise", json!({ "tuple": [43] })));
    insert(43, field("0", prim("u32"), true));
    insert(
        42,
        variant(
            "Custom",
            json!({ "struct": { "fields": [44], "has_stripped_fields": false } }),
        ),
    );
    insert(44, field("level", prim("u8"), true));

    json!({
        "index": index,
        "paths": {
            "1": { "crate_id": 0, "path": ["acme", "normalize"], "kind": "function" },
            "5": { "crate_id": 0, "path": ["acme", "with_time"], "kind": "function" },
            "6": { "crate_id": 0, "path": ["acme", "digest"], "kind": "function" },
            "7": { "crate_id": 0, "path": ["acme", "index_all"], "kind": "function" },
            "8": { "crate_id": 0, "path": ["acme", "merge"], "kind": "function" },
            "9": { "crate_id": 0, "path": ["acme", "trim"], "kind": "function" },
            "11": { "crate_id": 0, "path": ["acme", "stamp"], "kind": "function" },
            "2": { "crate_id": 0, "path": ["acme", "Counter"], "kind": "struct" },
            "3": { "crate_id": 0, "path": ["acme", "Mode"], "kind": "enum" },
            "4": { "crate_id": 0, "path": ["acme", "Level"], "kind": "enum" },
            "10": { "crate_id": 1, "path": ["chrono", "DateTime"], "kind": "struct" },
        },
    })
}

pub fn record(id: &str, kind: &str) -> ExportRecord {
    ExportRecord {
        id: id.into(),
        kind: kind.into(),
        fingerprint: 1,
        ..ExportRecord::default()
    }
}

pub fn method(id: &str, owner: &str) -> ExportRecord {
    ExportRecord {
        owner: Some(owner.into()),
        ..record(id, "method")
    }
}

pub fn records() -> Vec<ExportRecord> {
    vec![
        record("acme::normalize", "fn"),
        record("acme::with_time", "fn"),
        record("acme::digest", "fn"),
        record("acme::index_all", "fn"),
        record("acme::merge", "fn"),
        record("acme::trim", "fn"),
        record("acme::stamp", "fn"),
        record("acme::Counter", "struct"),
        record("acme::Mode", "enum"),
        record("acme::Level", "enum"),
        method("acme::Counter::new", "Counter"),
        method("acme::Counter::bump", "Counter"),
        method("acme::Counter::bump_all", "Counter"),
        method("acme::Counter::at", "Counter"),
        method("acme::Counter::consume", "Counter"),
        method("acme::Counter::refresh", "Counter"),
    ]
}

pub fn walk() -> (Surface, Vec<Gap>) {
    surface(&doc(), &TypeTable::with_defaults(), &records()).expect("walks")
}

pub fn opts() -> BindOptions {
    BindOptions {
        package: "acme-core".into(),
        module: "acme_core".into(),
        version: "0.1.0".into(),
        crate_name: "acme".into(),
        crate_package: "acme".into(),
        ..BindOptions::default()
    }
}

pub fn plan_for(kind: BindKind) -> BindingPlan {
    let (surface, gaps) = walk();
    lower(&surface, gaps, &opts(), kind).expect("lowers")
}
