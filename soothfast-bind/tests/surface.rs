//! Exported surface and wrapper plan over the shared fixture.

mod fixture;

use fixture::{doc, opts, plan_for, records, walk};
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
fn a_consuming_receiver_is_reported_and_left_unbound() {
    let (_, gaps) = walk();
    assert!(
        gaps.iter()
            .any(|g| matches!(g, Gap::ConsumingReceiver { at } if at == "acme::Counter::consume"))
    );
    let plan = plan_for(BindKind::Python);
    let counter = plan.classes.iter().find(|c| c.name == "Counter").unwrap();
    assert!(!counter.methods.iter().any(|m| m.name == "consume"));
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
        if r.id == "acme::Counter::refresh" {
            r.constructor = true;
        }
    }
    let (surface, gaps) = surface(&doc(), &TypeTable::with_defaults(), &records).expect("walks");
    let plan = lower(&surface, gaps, &opts(), BindKind::Python).expect("lowers");
    let counter = plan.classes.iter().find(|c| c.name == "Counter").unwrap();
    assert_eq!(
        counter.ctor.as_ref().map(|c| c.name.as_str()),
        Some("refresh")
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
