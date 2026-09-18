use soothfast_bind::BindKind;

use crate::{check_goldens, emit, emit_set};

#[test]
fn node_goldens() {
    check_goldens(BindKind::Node, "node");
}

#[test]
fn a_failing_call_throws_through_a_local_error_newtype() {
    let glue = emit(BindKind::Node)["src/lib.rs"].clone();
    assert!(glue.contains("struct BindErrorString(::std::string::String);"));
    assert!(glue.contains("impl ::std::convert::From<BindErrorString> for ::napi::Error"));
    assert!(glue.contains(".map_err(BindErrorString)?"));
}

#[test]
fn a_zero_copy_note_reports_where_node_could_avoid_a_copy() {
    let node = emit_set(BindKind::Node).notes.join("\n");
    assert!(node.contains("normalize: returning `Vec<f64>` allocates"));
}

#[test]
fn a_bigint_outside_range_fails_the_call_instead_of_truncating() {
    let glue = emit(BindKind::Node)["src/lib.rs"].clone();
    assert!(glue.contains("fn bigint_to_i64(value: ::napi::bindgen_prelude::BigInt"));
    assert!(glue.contains("fn bigint_to_u64(value: ::napi::bindgen_prelude::BigInt"));
    assert!(glue.contains("let start = bigint_to_i64(start, \"start\")?;"));
    assert!(glue.contains("pub fn new(start: BigInt) -> Result<Self>"));
    assert!(glue.contains("pub fn set_value(&mut self, value: BigInt) -> Result<()>"));
}

#[test]
fn a_borrowed_string_crosses_by_reference_for_node() {
    let glue = emit(BindKind::Node)["src/lib.rs"].clone();
    assert!(glue.contains("pub fn greet(name: String) -> String"));
    assert!(
        glue.contains("::acme::greet(&name)"),
        "napi has no FromNapiValue for &str, but the callee still needs a \
         reference to what napi hands it as an owned String"
    );
}

#[test]
fn an_async_method_is_reported_rather_than_bound_for_node() {
    let set = emit_set(BindKind::Node);
    assert!(!set.files["src/lib.rs"].contains("fn refresh"));
    assert!(
        set.gaps
            .iter()
            .any(|g| g.contains("no Node runtime story yet"))
    );
}

#[test]
fn an_optional_handle_return_maps_into_its_wrapper_for_node() {
    let glue = emit(BindKind::Node)["src/lib.rs"].clone();
    assert!(glue.contains("pub fn find_counter(start: BigInt) -> Result<Option<Counter>>"));
    assert!(glue.contains("Ok(::acme::find_counter(start).map(Counter))"));
}
