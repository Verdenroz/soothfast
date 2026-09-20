//! Exported surface and wrapper plan over the shared fixture.

mod fixture;

use fixture::{doc, module, opts, plan_for, records, reexport, stamped, walk};
use serde_json::json;
use soothfast_bind::BindKind;
use soothfast_bind::foreign::TypeTable;
use soothfast_bind::gap::Gap;
use soothfast_bind::model::{Ownership, Receiver, Surface, Ty, TypeKind, VariantFields};
use soothfast_bind::plan::{BindingPlan, lower};
use soothfast_bind::walk::surface;

fn find_fn<'a>(surface: &'a Surface, id: &str) -> &'a soothfast_bind::model::ExportedFn {
    surface
        .fns
        .iter()
        .find(|f| f.id == id)
        .unwrap_or_else(|| panic!("{id} not walked"))
}

#[test]
fn a_free_fn_reads_its_parameters_and_return() {
    let (surface, _) = walk();
    let f = find_fn(&surface, "acme::normalize");
    assert_eq!(f.receiver, Receiver::None);
    assert_eq!(f.params.len(), 2);
    assert_eq!(f.params[0].name, "input");
    assert_eq!(f.params[0].ty, Ty::List(Box::new(Ty::F64)));
    assert_eq!(f.params[0].ownership, Ownership::Owned);
    assert_eq!(f.ret, Ty::List(Box::new(Ty::F64)));
    assert_eq!(f.throws, None);
}

#[test]
fn a_result_return_splits_into_a_value_and_a_raise() {
    let (surface, _) = walk();
    let f = find_fn(&surface, "acme::Counter::bump");
    assert_eq!(f.ret, Ty::I64);
    assert_eq!(f.throws, Some(Ty::Str));
}

#[test]
fn a_self_return_names_the_type_it_builds() {
    let (surface, _) = walk();
    assert_eq!(
        find_fn(&surface, "acme::Counter::new").ret,
        Ty::Class("Counter".into())
    );
}

#[test]
fn receivers_are_told_apart_and_never_become_parameters() {
    let (surface, _) = walk();
    assert_eq!(
        find_fn(&surface, "acme::Counter::bump").receiver,
        Receiver::Shared
    );
    assert_eq!(
        find_fn(&surface, "acme::Counter::consume").receiver,
        Receiver::Consuming
    );
    assert_eq!(find_fn(&surface, "acme::Counter::bump").params.len(), 1);
}

#[test]
fn byte_sequences_never_become_lists_of_numbers() {
    let (surface, _) = walk();
    let f = find_fn(&surface, "acme::digest");
    assert_eq!(f.params[0].ty, Ty::Bytes);
    assert_eq!(f.params[0].ownership, Ownership::Borrowed);
    assert_eq!(f.ret, Ty::Bytes);
}

#[test]
fn an_async_fn_is_recorded_as_one() {
    let (surface, _) = walk();
    assert!(find_fn(&surface, "acme::Counter::refresh").is_async);
    assert!(!find_fn(&surface, "acme::normalize").is_async);
}

#[test]
fn struct_fields_carry_their_visibility() {
    let (surface, _) = walk();
    let counter = surface.types.iter().find(|t| t.name == "Counter").unwrap();
    let TypeKind::Struct(fields) = &counter.kind else {
        panic!("expected a struct");
    };
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].name, "value");
    assert!(fields[0].public);
    assert_eq!(fields[1].name, "label");
    assert!(!fields[1].public);
}

#[test]
fn every_variant_shape_survives_the_walk() {
    let (surface, _) = walk();
    let mode = surface.types.iter().find(|t| t.name == "Mode").unwrap();
    let TypeKind::Enum(variants) = &mode.kind else {
        panic!("expected an enum");
    };
    assert_eq!(variants[0].fields, VariantFields::Unit);
    assert_eq!(variants[1].fields, VariantFields::Tuple(vec![Ty::U32]));
    let VariantFields::Named(fields) = &variants[2].fields else {
        panic!("expected named fields");
    };
    assert_eq!(fields[0].name, "level");
}

#[test]
fn an_unmapped_foreign_type_is_reported_and_never_guessed() {
    let (surface, gaps) = walk();
    assert_eq!(
        find_fn(&surface, "acme::with_time").params[0].ty,
        Ty::Opaque("chrono::DateTime".into())
    );
    assert!(gaps.iter().any(|g| matches!(
        g,
        Gap::UnmappedForeign { path, .. } if path == "chrono::DateTime"
    )));
}

#[test]
fn a_mapped_foreign_type_binds_instead_of_reporting() {
    let mut table = TypeTable::with_defaults();
    table.insert("chrono::DateTime", Ty::Str);
    let (surface, gaps) = surface(&doc(), &table, &records()).expect("walks");
    assert_eq!(find_fn(&surface, "acme::with_time").params[0].ty, Ty::Str);
    assert!(!gaps.iter().any(|g| g.at() == "acme::with_time"));
}

#[test]
fn a_consuming_receiver_is_reported_and_left_unbound_for_every_backend_but_python() {
    let (surface, gaps) = walk();
    let go = lower(&surface, gaps.clone(), &opts(), BindKind::Go).expect("lowers");
    assert!(
        go.gaps
            .iter()
            .any(|g| matches!(g, Gap::ConsumingReceiver { at } if at == "acme::Counter::consume"))
    );
    let counter = go.classes.iter().find(|c| c.name == "Counter").unwrap();
    assert!(!counter.methods.iter().any(|m| m.name == "consume"));

    let python = lower(&surface, gaps, &opts(), BindKind::Python).expect("lowers");
    assert!(
        !python
            .gaps
            .iter()
            .any(|g| matches!(g, Gap::ConsumingReceiver { .. }))
    );
    let counter = python.classes.iter().find(|c| c.name == "Counter").unwrap();
    assert!(counter.methods.iter().any(|m| m.name == "consume"));
}

#[test]
fn the_walk_is_independent_of_registry_order() {
    let mut reversed = records();
    reversed.reverse();
    let forward = surface(&doc(), &TypeTable::with_defaults(), &records()).expect("walks");
    let backward = surface(&doc(), &TypeTable::with_defaults(), &reversed).expect("walks");
    assert_eq!(forward.0, backward.0);
}

#[test]
fn an_inherent_new_becomes_the_constructor() {
    let plan = plan_for(BindKind::Python);
    let counter = plan.classes.iter().find(|c| c.name == "Counter").unwrap();
    let ctor = counter.ctor.as_ref().expect("has a constructor");
    assert_eq!(ctor.name, "new");
    assert!(!counter.statics.iter().any(|s| s.name == "new"));
}

#[test]
fn a_declared_constructor_wins_over_an_inherent_new() {
    let mut records = records();
    for r in &mut records {
        if r.id == "acme::Counter::bump_all" {
            r.constructor = true;
        }
    }
    let (surface, gaps) = surface(&doc(), &TypeTable::with_defaults(), &records).expect("walks");
    let plan = lower(&surface, gaps, &opts(), BindKind::Python).expect("lowers");
    let counter = plan.classes.iter().find(|c| c.name == "Counter").unwrap();
    assert_eq!(
        counter.ctor.as_ref().map(|c| c.name.as_str()),
        Some("bump_all")
    );
}

#[test]
fn only_public_fields_become_accessors() {
    let plan = plan_for(BindKind::Python);
    let counter = plan.classes.iter().find(|c| c.name == "Counter").unwrap();
    assert_eq!(counter.accessors.len(), 1);
    assert_eq!(counter.accessors[0].field, "value");
    assert_eq!(counter.accessors[0].ty, Ty::I64);
}

#[test]
fn symbols_are_stable_and_language_neutral() {
    let python = plan_for(BindKind::Python);
    let wasm = plan_for(BindKind::Wasm);
    let symbol = |p: &BindingPlan| {
        p.classes
            .iter()
            .find(|c| c.name == "Counter")
            .unwrap()
            .methods
            .iter()
            .find(|m| m.name == "bump")
            .unwrap()
            .symbol
            .clone()
    };
    assert_eq!(symbol(&python), "acme_core_counter_bump");
    assert_eq!(symbol(&python), symbol(&wasm));
}

#[test]
fn a_language_that_cannot_carry_a_type_reports_it_rather_than_degrading() {
    let wasm = plan_for(BindKind::Wasm);
    assert!(!wasm.functions.iter().any(|f| f.name == "index_all"));
    assert!(wasm.gaps.iter().any(|g| matches!(
        g,
        Gap::UnsupportedByBackend { at, lang, .. } if at == "acme::index_all" && *lang == "wasm"
    )));

    let python = plan_for(BindKind::Python);
    assert!(python.functions.iter().any(|f| f.name == "index_all"));
}

#[test]
fn a_skipped_language_leaves_the_item_out_of_that_plan_only() {
    let mut records = records();
    for r in &mut records {
        if r.id == "acme::normalize" {
            r.skip = vec!["wasm".into()];
        }
    }
    let build = |kind| {
        let (surface, gaps) =
            surface(&doc(), &TypeTable::with_defaults(), &records).expect("walks");
        lower(&surface, gaps, &opts(), kind).expect("lowers")
    };
    assert!(
        !build(BindKind::Wasm)
            .functions
            .iter()
            .any(|f| f.name == "normalize")
    );
    assert!(
        build(BindKind::Python)
            .functions
            .iter()
            .any(|f| f.name == "normalize")
    );
}

#[test]
fn lowering_the_same_surface_twice_gives_the_same_plan() {
    assert_eq!(plan_for(BindKind::Python), plan_for(BindKind::Python));
}

#[test]
fn an_exported_type_taken_by_value_is_reported_rather_than_copied() {
    let plan = plan_for(BindKind::Python);
    assert!(!plan.functions.iter().any(|f| f.name == "merge"));
    assert!(plan.gaps.iter().any(|g| matches!(
        g,
        Gap::HandleByValue { at, ty } if at == "acme::merge" && ty == "Counter"
    )));
}

/// A data-carrying enum always binds as an opaque handle (see
/// `pyo3::glue::plain_enum`'s counterpart for plain ones), so a variant's
/// own payload never crosses on any backend, mapped or not.
#[test]
fn an_unmapped_type_inside_a_variant_payload_is_not_gapped() {
    use fixture::{enum_item, field, path, record, variant};

    let doc = json!({
        "index": {
            "1": enum_item("Stamped", &[2], &[]),
            "2": variant("At", json!({ "tuple": [3] })),
            "3": field("0", path("Duration", 4, &[]), true),
        },
        "paths": {
            "1": { "crate_id": 0, "path": ["shape", "Stamped"], "kind": "enum" },
            "4": { "crate_id": 1, "path": ["std", "time", "Duration"], "kind": "struct" },
        },
    });
    let table = TypeTable::default();
    let records = vec![record("shape::Stamped", "enum")];
    let (surface, gaps) = surface(&doc, &table, &records).expect("walks");

    assert_eq!(surface.types.len(), 1);
    assert!(
        matches!(&surface.types[0].kind, TypeKind::Enum(v) if v[0].fields == VariantFields::Tuple(vec![Ty::Opaque("std::time::Duration".into())])),
        "{:?}",
        surface.types[0].kind
    );
    assert!(
        !gaps
            .iter()
            .any(|g| matches!(g, Gap::UnmappedForeign { .. })),
        "a variant's payload can never cross, so its unmapped type is not a gap: {gaps:?}"
    );
}

/// A struct declared in a private module and reached at the crate root only
/// through `pub use inner::Region as IndicesRegion` takes the alias as its
/// class name and public path, not the identifier it was declared under.
#[test]
fn a_root_reexport_alias_becomes_the_class_name() {
    use fixture::{record, struct_item};

    let doc = stamped(json!({
        "root": 0,
        "index": {
            "0": module("acme", true, &[3]),
            "2": struct_item("Region", &[], &[]),
            "3": reexport("IndicesRegion", 2),
        },
        "paths": {
            "2": { "crate_id": 0, "path": ["acme", "indices", "Region"], "kind": "struct" },
        },
    }));
    let table = TypeTable::default();
    let records = vec![record("acme::indices::Region", "struct")];
    let (surface, gaps) = surface(&doc, &table, &records).expect("walks");

    assert_eq!(surface.types.len(), 1);
    assert_eq!(surface.types[0].id, "acme::indices::Region");
    assert_eq!(surface.types[0].rust_path, "acme::IndicesRegion");
    assert_eq!(surface.types[0].name, "IndicesRegion");
    assert!(
        !gaps.iter().any(|g| matches!(g, Gap::NameCollision { .. })),
        "a lone alias has nothing to collide with: {gaps:?}"
    );
}

/// A field referencing an aliased type spells it as the source wrote it
/// (the pre-alias name); the class it resolves to is only ever registered
/// under the alias, so the field's type must resolve to that name too.
#[test]
fn a_field_of_an_aliased_type_resolves_to_the_alias() {
    use fixture::{field, path, record, struct_item};

    let doc = stamped(json!({
        "root": 0,
        "index": {
            "0": module("acme", true, &[3, 4]),
            "2": struct_item("Region", &[], &[]),
            "3": reexport("IndicesRegion", 2),
            "4": struct_item("Holder", &[5], &[]),
            "5": field("region", path("Region", 2, &[]), true),
        },
        "paths": {
            "2": { "crate_id": 0, "path": ["acme", "indices", "Region"], "kind": "struct" },
            "4": { "crate_id": 0, "path": ["acme", "Holder"], "kind": "struct" },
        },
    }));
    let table = TypeTable::default();
    let records = vec![
        record("acme::indices::Region", "struct"),
        record("acme::Holder", "struct"),
    ];
    let (surface, _) = surface(&doc, &table, &records).expect("walks");

    let holder = surface.types.iter().find(|t| t.name == "Holder").unwrap();
    let TypeKind::Struct(fields) = &holder.kind else {
        panic!("expected a struct");
    };
    assert_eq!(fields[0].ty, Ty::Class("IndicesRegion".into()));
}

/// Three exported types can end up with the same short name: two structs
/// both called `Report` in different modules, and a third at the crate
/// root. Only the root one — the shortest public path — keeps the name;
/// the other two are gaps rather than a glue file with three `Report`s.
#[test]
fn the_shortest_public_path_wins_a_three_way_name_collision() {
    use fixture::{record, struct_item};

    let doc = json!({
        "index": {
            "1": struct_item("Report", &[], &[]),
            "2": struct_item("Report", &[], &[]),
            "3": struct_item("Report", &[], &[]),
        },
        "paths": {
            "1": { "crate_id": 0, "path": ["acme", "Report"], "kind": "struct" },
            "2": { "crate_id": 0, "path": ["acme", "sectors", "Report"], "kind": "struct" },
            "3": { "crate_id": 0, "path": ["acme", "industries", "Report"], "kind": "struct" },
        },
    });
    let table = TypeTable::default();
    let records = vec![
        record("acme::Report", "struct"),
        record("acme::sectors::Report", "struct"),
        record("acme::industries::Report", "struct"),
    ];
    let (surface, gaps) = surface(&doc, &table, &records).expect("walks");

    assert_eq!(
        surface.types.iter().map(|t| &t.id).collect::<Vec<_>>(),
        vec!["acme::Report"],
        "only the shortest path is walked as a type"
    );
    for loser in ["acme::sectors::Report", "acme::industries::Report"] {
        assert!(
            gaps.iter().any(|g| matches!(
                g,
                Gap::NameCollision { at, name, winner }
                    if at == loser && name == "Report" && winner == "acme::Report"
            )),
            "{loser} should lose to acme::Report: {gaps:?}"
        );
    }
}

/// A struct declared in a private module, with no `pub use` reaching it,
/// cannot be named from outside the crate: it is reported rather than
/// walked, since glue that named it by its declaration path would not
/// compile.
#[test]
fn a_type_with_no_public_path_is_reported_rather_than_walked() {
    use fixture::{record, struct_item};

    let doc = stamped(json!({
        "root": 0,
        "index": {
            "0": module("acme", true, &[1]),
            "1": module("hidden", false, &[2]),
            "2": struct_item("Secret", &[], &[]),
        },
        "paths": {
            "2": { "crate_id": 0, "path": ["acme", "hidden", "Secret"], "kind": "struct" },
        },
    }));
    let table = TypeTable::default();
    let records = vec![record("acme::hidden::Secret", "struct")];
    let (surface, gaps) = surface(&doc, &table, &records).expect("walks");

    assert!(surface.types.is_empty(), "{:?}", surface.types);
    assert!(
        gaps.iter()
            .any(|g| matches!(g, Gap::Unreachable { at } if at == "acme::hidden::Secret")),
        "{gaps:?}"
    );
}

/// An exported generic struct or enum has no single Rust path a binding can
/// name it by, so it is reported rather than emitted as `finance_query::T`
/// with its type argument silently dropped.
#[test]
fn a_generic_type_is_reported_rather_than_walked() {
    use fixture::record;
    use serde_json::Value;

    let doc = json!({
        "index": {
            "1": {
                "name": "Wrapper", "docs": Value::Null, "attrs": [], "visibility": "public",
                "inner": { "struct": {
                    "kind": { "plain": { "fields": [], "has_stripped_fields": false } },
                    "generics": { "params": [
                        { "name": "T", "kind": { "type": {
                            "bounds": [], "default": Value::Null, "is_synthetic": false } } }
                    ], "where_predicates": [] },
                    "impls": [] } },
            },
        },
        "paths": {
            "1": { "crate_id": 0, "path": ["acme", "Wrapper"], "kind": "struct" },
        },
    });
    let table = TypeTable::default();
    let records = vec![record("acme::Wrapper", "struct")];
    let (surface, gaps) = surface(&doc, &table, &records).expect("walks");

    assert!(surface.types.is_empty(), "{:?}", surface.types);
    assert!(
        gaps.iter().any(
            |g| matches!(g, Gap::Generic { at, param } if at == "acme::Wrapper" && param == "T")
        ),
        "{gaps:?}"
    );
}

/// A bare lifetime parameter is reported the same way: nothing outlives a
/// handle held across a Python call, so a borrowed type is no better bound
/// than a generic one.
#[test]
fn a_type_with_a_lifetime_parameter_is_reported_rather_than_walked() {
    use fixture::record;
    use serde_json::Value;

    let doc = json!({
        "index": {
            "1": {
                "name": "Borrowed", "docs": Value::Null, "attrs": [], "visibility": "public",
                "inner": { "struct": {
                    "kind": { "plain": { "fields": [], "has_stripped_fields": false } },
                    "generics": { "params": [
                        { "name": "'a", "kind": { "lifetime": { "outlives": [] } } }
                    ], "where_predicates": [] },
                    "impls": [] } },
            },
        },
        "paths": {
            "1": { "crate_id": 0, "path": ["acme", "Borrowed"], "kind": "struct" },
        },
    });
    let table = TypeTable::default();
    let records = vec![record("acme::Borrowed", "struct")];
    let (surface, gaps) = surface(&doc, &table, &records).expect("walks");

    assert!(surface.types.is_empty(), "{:?}", surface.types);
    assert!(
        gaps.iter().any(
            |g| matches!(g, Gap::Generic { at, param } if at == "acme::Borrowed" && param == "'a")
        ),
        "{gaps:?}"
    );
}

/// A method's registry id carries the module its own `impl` block sits in,
/// not the module the type it is on is declared in — the two only match
/// when every impl block sits beside the struct. When they don't, the owner
/// has to be found by name among the already-exported types instead of by
/// splitting the method's own id, or every method on the type is reported
/// as an unmapped reference to itself.
#[test]
fn a_method_declared_outside_its_types_own_module_still_binds() {
    use fixture::{borrowed, func, method, prim, record, struct_item};
    use serde_json::Value;

    let doc = stamped(json!({
        "root": 0,
        "index": {
            "0": module("acme", true, &[2]),
            "2": struct_item("Basket", &[], &[30]),
            "30": { "name": Value::Null, "docs": Value::Null, "attrs": [],
                    "inner": { "impl": { "trait": Value::Null, "items": [31] } } },
            "31": func(
                "total",
                &[("self", borrowed(json!({ "generic": "Self" }), false))],
                prim("i64"),
                false,
            ),
        },
        "paths": {
            "2": { "crate_id": 0, "path": ["acme", "Basket"], "kind": "struct" },
        },
    }));
    let table = TypeTable::default();
    let records = vec![
        record("acme::Basket", "struct"),
        method("acme::batch::Basket::total", "Basket"),
    ];
    let (surface, gaps) = surface(&doc, &table, &records).expect("walks");

    let f = surface
        .fns
        .iter()
        .find(|f| f.id == "acme::batch::Basket::total")
        .unwrap_or_else(|| panic!("total not walked: {gaps:?}"));
    assert_eq!(f.rust_path, "acme::Basket::total");
    assert!(gaps.is_empty(), "{gaps:?}");
}

/// Two exported types can share a bare name without colliding: one is
/// reached only through a root alias, so `NameCollision` never sees them as
/// the same public name. A method whose id prefix matches neither type's
/// own module then has two equally bare-named candidates to attach to;
/// guessing either one would silently steal the method from the other, so
/// it is gapped instead.
#[test]
fn a_method_naming_neither_of_two_same_named_types_is_gapped() {
    use fixture::{method, record, reexport, struct_item};

    let doc = stamped(json!({
        "root": 0,
        "index": {
            "0": module("acme", true, &[3, 4]),
            "2": struct_item("Region", &[], &[]),
            "3": reexport("IndicesRegion", 2),
            "4": struct_item("Region", &[], &[]),
        },
        "paths": {
            "2": { "crate_id": 0, "path": ["acme", "indices", "Region"], "kind": "struct" },
            "4": { "crate_id": 0, "path": ["acme", "Region"], "kind": "struct" },
        },
    }));
    let table = TypeTable::default();
    let records = vec![
        record("acme::indices::Region", "struct"),
        record("acme::Region", "struct"),
        method("acme::other::Region::code", "Region"),
    ];
    let (surface, gaps) = surface(&doc, &table, &records).expect("walks");

    assert!(surface.fns.is_empty(), "{:?}", surface.fns);
    assert!(
        gaps.iter().any(|g| matches!(
            g,
            Gap::UnmappedForeign { at, path }
                if at == "acme::other::Region::code" && path == "acme::other::Region::code"
        )),
        "{gaps:?}"
    );
}
