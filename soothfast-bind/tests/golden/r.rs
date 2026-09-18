use soothfast_bind::BindKind;

use crate::fixture::r_opts;
use crate::{check_goldens_with, emit_set_with};

#[test]
fn r_goldens() {
    check_goldens_with(BindKind::R, "r", &r_opts());
}

#[test]
fn the_r_manifest_steps_up_past_src_rust_to_the_package_root() {
    let manifest = &emit_set_with(BindKind::R, &r_opts()).files["src/rust/Cargo.toml"];
    assert!(manifest.contains("acme = { path = \"../../..\" }"));
}

#[test]
fn a_64_bit_integer_gets_one_note_about_crossing_as_a_checked_double() {
    let notes = emit_set_with(BindKind::R, &r_opts()).notes;
    let checked: Vec<&String> = notes
        .iter()
        .filter(|n| n.contains("R has no 64-bit integer"))
        .collect();
    assert_eq!(checked.len(), 1, "expected exactly one note: {notes:?}");
}

#[test]
fn a_failing_call_raises_an_r_condition_with_the_rust_message() {
    let glue = &emit_set_with(BindKind::R, &r_opts()).files["src/rust/src/lib.rs"];
    assert!(glue.contains("Err(reason) => Err(::std::string::ToString::to_string(&reason)),"));
}

#[test]
fn a_plain_enum_crosses_as_a_validated_string_not_an_ordinal() {
    let glue = &emit_set_with(BindKind::R, &r_opts()).files["src/rust/src/lib.rs"];
    assert!(glue.contains("\"Low\" => Inner::Low,"));
    assert!(glue.contains("\"High\" => Inner::High,"));
    assert!(glue.contains("unknown Level variant"));
}

#[test]
fn an_async_method_is_a_gap_with_no_r_runtime_story() {
    let set = emit_set_with(BindKind::R, &r_opts());
    assert!(
        set.gaps
            .iter()
            .any(|g| g.contains("no R runtime story yet")),
        "gaps: {:?}",
        set.gaps
    );
    assert!(!set.files["src/rust/src/lib.rs"].contains("fn refresh"));
}

#[test]
fn an_option_of_a_sequence_binds_where_c_gaps_it() {
    let set = emit_set_with(BindKind::R, &r_opts());
    assert!(!set.gaps.iter().any(|g| g.contains("trim")));
    let glue = &set.files["src/rust/src/lib.rs"];
    assert!(glue.contains("fn trim(input: &[f64]) -> Robj {"));
}

#[test]
fn an_optional_string_crosses_as_null_not_na_in_r() {
    let set = emit_set_with(BindKind::R, &r_opts());
    assert!(!set.gaps.iter().any(|g| g.contains("describe")));
    let glue = &set.files["src/rust/src/lib.rs"];
    assert!(glue.contains("fn describe(label: Robj) -> ::std::result::Result<Robj, String> {"));
    assert!(glue.contains("Some(s) if s.is_na() => None,"));
    assert!(glue.contains("None if label.is_null() => None,"));
    assert!(glue.contains("Some(v) => Robj::from(v), None => ().into()"));
}

#[test]
fn a_same_class_parameter_checks_pointer_identity_against_self() {
    let glue = &emit_set_with(BindKind::R, &r_opts()).files["src/rust/src/lib.rs"];
    assert!(glue.contains(
        "fn absorb(&mut self, other: &Counter) -> ::std::result::Result<(), String> {\n        \
         if ::std::ptr::eq(self, other) {\n            \
         return Err(\"other aliases self\".to_string());\n        }"
    ));
}

#[test]
fn an_accessor_gets_a_replacement_function_as_well_as_a_getter() {
    let set = emit_set_with(BindKind::R, &r_opts());
    let glue = &set.files["src/rust/src/lib.rs"];
    assert!(
        glue.contains("fn set_value(&mut self, value: f64) -> ::std::result::Result<(), String>")
    );
    let wrappers = &set.files["R/acme.core.R"];
    assert!(wrappers.contains(
        "Counter__set_value <- function(self, value) .Call(wrap__Counter__set_value, self, value)"
    ));
    assert!(wrappers.contains("`value<-` <- function(x, value) UseMethod(\"value<-\")"));
    assert!(wrappers.contains(
        "`value<-.Counter` <- function(x, value) {\n  Counter__set_value(x, value)\n  x\n}"
    ));
    let namespace = &set.files["NAMESPACE"];
    assert!(namespace.contains("export(\"value<-\")"));
    assert!(namespace.contains("S3method(\"value<-\", Counter)"));
}

#[test]
fn a_mutable_out_parameter_is_a_gap_since_r_vectors_are_values() {
    let set = emit_set_with(BindKind::R, &r_opts());
    assert!(
        set.gaps
            .iter()
            .any(|g| g.contains("scale_into") && g.contains("R vectors are values")),
        "gaps: {:?}",
        set.gaps
    );
    assert!(!set.files["src/rust/src/lib.rs"].contains("fn scale_into"));
}
