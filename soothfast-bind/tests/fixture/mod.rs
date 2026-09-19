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

pub fn enum_item(name: &str, variants: &[u64], impls: &[u64]) -> Value {
    json!({
        "name": name, "docs": Value::Null, "attrs": [], "visibility": "public",
        "inner": { "enum": {
            "variants": variants,
            "generics": { "params": [], "where_predicates": [] },
            "impls": impls } },
    })
}

pub fn variant(name: &str, kind: Value) -> Value {
    json!({
        "name": name, "docs": Value::Null, "attrs": [],
        "inner": { "variant": { "kind": kind } },
    })
}

/// The crate's free functions: `normalize` through `maybe_ratio`.
fn free_functions(insert: &mut impl FnMut(u64, Value)) {
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
    insert(
        12,
        func(
            "greet",
            &[("name", borrowed(prim("str"), false))],
            path("String", 92, &[]),
            false,
        ),
    );
    insert(
        13,
        func(
            "scale_into",
            &[
                ("values", borrowed(json!({ "slice": prim("f64") }), false)),
                ("factor", prim("f64")),
                ("out", borrowed(json!({ "slice": prim("f64") }), true)),
            ],
            Value::Null,
            false,
        ),
    );
    insert(
        14,
        func(
            "peak_level",
            &[("values", borrowed(json!({ "slice": prim("f64") }), false))],
            path("Level", 4, &[]),
            false,
        ),
    );
    insert(
        15,
        func(
            "find_counter",
            &[("start", prim("i64"))],
            path("Option", 93, &[path("Counter", 2, &[])]),
            false,
        ),
    );
    insert(
        53,
        func(
            "is_high",
            &[("level", borrowed(path("Level", 4, &[]), false))],
            prim("bool"),
            false,
        ),
    );
    insert(
        22,
        func(
            "mutate_counter",
            &[("counter", borrowed(path("Counter", 2, &[]), true))],
            prim("i64"),
            false,
        ),
    );
    insert(
        54,
        func(
            "flags",
            &[("values", path("Vec", 90, &[prim("bool")]))],
            path("Vec", 90, &[prim("bool")]),
            false,
        ),
    );
    insert(
        55,
        func(
            "sample_ids",
            &[("ids", path("Vec", 90, &[prim("usize")]))],
            path("Vec", 90, &[prim("usize")]),
            false,
        ),
    );
    insert(
        28,
        func(
            "levels",
            &[],
            path("Vec", 90, &[path("Level", 4, &[])]),
            false,
        ),
    );
    insert(
        29,
        func(
            "counters",
            &[],
            path("Vec", 90, &[path("Counter", 2, &[])]),
            false,
        ),
    );
}

/// The crate's free functions that take or return an `Option`.
fn optional_free_functions(insert: &mut impl FnMut(u64, Value)) {
    insert(
        16,
        func(
            "describe",
            &[("label", path("Option", 93, &[borrowed(prim("str"), false)]))],
            path("Option", 93, &[path("String", 92, &[])]),
            false,
        ),
    );

    insert(
        17,
        func(
            "describe_owned",
            &[("label", path("Option", 93, &[path("String", 92, &[])]))],
            path("Option", 93, &[path("String", 92, &[])]),
            false,
        ),
    );

    insert(
        18,
        func(
            "maybe_ratio",
            &[("value", prim("f64"))],
            path("Option", 93, &[prim("f64")]),
            false,
        ),
    );
    insert(
        24,
        func(
            "scale_optional",
            &[("factor", path("Option", 93, &[prim("f64")]))],
            prim("f64"),
            false,
        ),
    );
    insert(
        25,
        func(
            "sum_optional",
            &[(
                "values",
                path("Option", 93, &[path("Vec", 90, &[prim("f64")])]),
            )],
            prim("f64"),
            false,
        ),
    );
    insert(
        26,
        func(
            "set_level",
            &[("level", path("Option", 93, &[path("Level", 4, &[])]))],
            prim("bool"),
            false,
        ),
    );
    insert(
        27,
        func(
            "peek",
            &[(
                "counter",
                path("Option", 93, &[borrowed(path("Counter", 2, &[]), false)]),
            )],
            prim("bool"),
            false,
        ),
    );
}

/// The crate's free functions that always fail, for exercising the error
/// path itself rather than any particular return shape.
fn failing_free_functions(insert: &mut impl FnMut(u64, Value)) {
    insert(
        19,
        func(
            "fail",
            &[("message", borrowed(prim("str"), false))],
            path("Result", 93, &[prim("i64"), path("String", 92, &[])]),
            false,
        ),
    );
}

/// The crate's free functions taking more than one writable buffer, so a
/// backend that can only keep one pinned at a time has a second to prove it
/// still writes back.
fn multi_buffer_free_functions(insert: &mut impl FnMut(u64, Value)) {
    insert(
        23,
        func(
            "split",
            &[
                ("src", borrowed(json!({ "slice": prim("f64") }), false)),
                ("lo", borrowed(json!({ "slice": prim("f64") }), true)),
                ("hi", borrowed(json!({ "slice": prim("f64") }), true)),
            ],
            Value::Null,
            false,
        ),
    );
}

/// `Counter`, its fields and its auto-trait impls. The inherent impl block
/// and its methods are [`methods`]'s to insert.
fn structs(insert: &mut impl FnMut(u64, Value)) {
    insert(2, struct_item("Counter", &[20, 21], &[30, 37, 38]));
    insert(37, auto_impl("Send", false));
    insert(38, auto_impl("Sync", false));
    insert(20, field("value", prim("i64"), true));
    insert(21, field("label", path("String", 92, &[]), false));
}

/// `Counter`'s inherent impl block and every method on it.
fn methods(insert: &mut impl FnMut(u64, Value)) {
    insert(
        30,
        json!({ "name": Value::Null, "docs": Value::Null, "attrs": [],
                "inner": { "impl": { "trait": Value::Null, "items": [31, 32, 33, 34, 35, 36, 51, 52] } } }),
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
    // An exclusive receiver: the one shape every other method here avoids.
    insert(
        51,
        func(
            "scale",
            &[
                ("self", borrowed(json!({ "generic": "Self" }), true)),
                ("factor", prim("i64")),
            ],
            prim("i64"),
            false,
        ),
    );
    // A parameter of the receiver's own class, so `x.absorb(x)` can alias
    // the same object a backend's `&mut self` already borrows.
    insert(
        52,
        func(
            "absorb",
            &[
                ("self", borrowed(json!({ "generic": "Self" }), true)),
                ("other", borrowed(path("Counter", 2, &[]), false)),
            ],
            Value::Null,
            false,
        ),
    );
}

/// `Level`, a payload-free enum, and `Mode`, one that carries data. Real
/// rustdoc emits Send/Sync auto-impls for both, the same as it does for
/// `Counter`.
fn enums(insert: &mut impl FnMut(u64, Value)) {
    insert(4, enum_item("Level", &[45, 46], &[47, 48]));
    insert(45, variant("Low", json!("plain")));
    insert(46, variant("High", json!("plain")));
    insert(47, auto_impl("Send", false));
    insert(48, auto_impl("Sync", false));

    insert(3, enum_item("Mode", &[40, 41, 42], &[49, 50]));
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
    // Rust has no per-field visibility inside an enum variant, so rustdoc
    // always reports "default" here regardless of the field's own privacy.
    insert(44, field("level", prim("u8"), false));
    insert(49, auto_impl("Send", false));
    insert(50, auto_impl("Sync", false));
}

/// `acme`, with a free fn, a struct and its inherent impl, and an enum.
pub fn doc() -> Value {
    let mut index = Map::new();
    let mut insert = |id: u64, item: Value| {
        index.insert(id.to_string(), item);
    };

    free_functions(&mut insert);
    optional_free_functions(&mut insert);
    failing_free_functions(&mut insert);
    multi_buffer_free_functions(&mut insert);
    structs(&mut insert);
    methods(&mut insert);
    enums(&mut insert);

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
            "12": { "crate_id": 0, "path": ["acme", "greet"], "kind": "function" },
            "13": { "crate_id": 0, "path": ["acme", "scale_into"], "kind": "function" },
            "14": { "crate_id": 0, "path": ["acme", "peak_level"], "kind": "function" },
            "15": { "crate_id": 0, "path": ["acme", "find_counter"], "kind": "function" },
            "53": { "crate_id": 0, "path": ["acme", "is_high"], "kind": "function" },
            "22": { "crate_id": 0, "path": ["acme", "mutate_counter"], "kind": "function" },
            "54": { "crate_id": 0, "path": ["acme", "flags"], "kind": "function" },
            "55": { "crate_id": 0, "path": ["acme", "sample_ids"], "kind": "function" },
            "28": { "crate_id": 0, "path": ["acme", "levels"], "kind": "function" },
            "29": { "crate_id": 0, "path": ["acme", "counters"], "kind": "function" },
            "16": { "crate_id": 0, "path": ["acme", "describe"], "kind": "function" },
            "17": { "crate_id": 0, "path": ["acme", "describe_owned"], "kind": "function" },
            "18": { "crate_id": 0, "path": ["acme", "maybe_ratio"], "kind": "function" },
            "19": { "crate_id": 0, "path": ["acme", "fail"], "kind": "function" },
            "23": { "crate_id": 0, "path": ["acme", "split"], "kind": "function" },
            "24": { "crate_id": 0, "path": ["acme", "scale_optional"], "kind": "function" },
            "25": { "crate_id": 0, "path": ["acme", "sum_optional"], "kind": "function" },
            "26": { "crate_id": 0, "path": ["acme", "set_level"], "kind": "function" },
            "27": { "crate_id": 0, "path": ["acme", "peek"], "kind": "function" },
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
        record("acme::greet", "fn"),
        record("acme::scale_into", "fn"),
        record("acme::peak_level", "fn"),
        record("acme::find_counter", "fn"),
        record("acme::is_high", "fn"),
        record("acme::mutate_counter", "fn"),
        record("acme::flags", "fn"),
        record("acme::sample_ids", "fn"),
        record("acme::levels", "fn"),
        record("acme::counters", "fn"),
        record("acme::describe", "fn"),
        record("acme::describe_owned", "fn"),
        record("acme::maybe_ratio", "fn"),
        record("acme::fail", "fn"),
        record("acme::split", "fn"),
        record("acme::scale_optional", "fn"),
        record("acme::sum_optional", "fn"),
        record("acme::set_level", "fn"),
        record("acme::peek", "fn"),
        record("acme::Counter", "struct"),
        record("acme::Mode", "enum"),
        record("acme::Level", "enum"),
        method("acme::Counter::new", "Counter"),
        method("acme::Counter::bump", "Counter"),
        method("acme::Counter::bump_all", "Counter"),
        method("acme::Counter::at", "Counter"),
        method("acme::Counter::scale", "Counter"),
        method("acme::Counter::absorb", "Counter"),
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

/// `opts()`, but with `package` as a Go module path rather than a Cargo
/// distribution name, the way a `[[bind]] lang = "go"` entry configures it.
/// `module` is left as `[[bind]]` would default it: hyphens replaced, dots
/// and slashes untouched, since that default is what the CLI actually hands
/// the backend when a config leaves `module` unset.
pub fn go_opts() -> BindOptions {
    let package = "github.com/acme/core";
    BindOptions {
        package: package.into(),
        module: package.replace('-', "_"),
        ..opts()
    }
}

pub fn plan_for(kind: BindKind) -> BindingPlan {
    let (surface, gaps) = walk();
    lower(&surface, gaps, &opts(), kind).expect("lowers")
}

/// `opts()`'s `package` is a distribution name with a hyphen, which every
/// other backend accepts but Java cannot: `[[bind]] package` there is a
/// dotted Java package name instead.
pub fn java_opts() -> BindOptions {
    BindOptions {
        package: "acme.core".into(),
        ..opts()
    }
}

/// The same dotted package as [`java_opts`]: Kotlin shares Java's JNI glue,
/// so pointing both at the same package is what makes the glue-identity
/// test between them meaningful.
pub fn kotlin_opts() -> BindOptions {
    java_opts()
}

/// An R package name: letters, digits and dots, no hyphens.
pub fn r_opts() -> BindOptions {
    BindOptions {
        package: "acme.core".into(),
        ..opts()
    }
}

/// `opts()`'s own `package`/`module` split already matches what a gem
/// needs: `acme-core` is the gem name, `acme_core` the requirable path.
pub fn ruby_opts() -> BindOptions {
    opts()
}

/// `opts()`'s `package` is a Cargo distribution name; a `[[bind]] lang =
/// "cpp"` entry gives it a `::`-delimited C++ namespace path instead.
/// `module` is left as `[[bind]]` would default it: hyphens replaced, `::`
/// untouched, since that default is what the CLI actually hands the
/// backend when a config leaves `module` unset.
pub fn cpp_opts() -> BindOptions {
    let package = "acme::core";
    BindOptions {
        package: package.into(),
        module: package.replace('-', "_"),
        ..opts()
    }
}

/// A dotted `require` path, the way `[[bind]] lang = "lua"` configures
/// `package`: unlike every other backend, this is not a distribution name.
/// `module` is left as `[[bind]]` would default it, since the backend
/// discards it anyway in favor of the path's own last segment.
pub fn lua_opts() -> BindOptions {
    BindOptions {
        package: "acme.core".into(),
        module: "acme.core".into(),
        ..opts()
    }
}

/// A dotted root namespace, the way a `[[bind]] lang = "csharp"` entry
/// configures `package`.
pub fn csharp_opts() -> BindOptions {
    BindOptions {
        package: "Acme.Core".into(),
        ..opts()
    }
}
