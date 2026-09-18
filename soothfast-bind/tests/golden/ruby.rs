use soothfast_bind::BindKind;

use crate::fixture::ruby_opts;
use crate::{check_goldens_with, emit_set_with};

#[test]
fn ruby_goldens() {
    check_goldens_with(BindKind::Ruby, "ruby", &ruby_opts());
}

#[test]
fn the_ruby_manifest_steps_up_past_ext_module_to_the_package_root() {
    let manifest = &emit_set_with(BindKind::Ruby, &ruby_opts()).files["ext/acme_core/Cargo.toml"];
    assert!(manifest.contains("acme = { path = \"../../..\" }"));
}

#[test]
fn a_ruby_buffer_call_copies_with_no_transfer_notes() {
    let notes = emit_set_with(BindKind::Ruby, &ruby_opts()).notes;
    assert!(
        notes
            .iter()
            .all(|n| !n.contains("would arrive without a copy")
                && !n.contains("allocates a fresh sequence")),
        "ruby copies every buffer regardless of the signature: {notes:?}"
    );
}

#[test]
fn a_failing_ruby_call_raises_the_package_error() {
    let glue =
        emit_set_with(BindKind::Ruby, &ruby_opts()).files["ext/acme_core/src/lib.rs"].clone();
    assert!(glue.contains("define_error(\"Error\", ruby.exception_standard_error())"));
    assert!(glue.contains("::magnus::Error::new(ruby.get_inner(&ERROR),"));
}

#[test]
fn a_ruby_plain_enum_crosses_as_a_validated_symbol() {
    let glue =
        emit_set_with(BindKind::Ruby, &ruby_opts()).files["ext/acme_core/src/lib.rs"].clone();
    assert!(glue.contains("fn level_from_symbol("));
    assert!(glue.contains("\"low\" => Ok(::acme::Level::Low),"));
    assert!(glue.contains("ruby.exception_arg_error()"));
    assert!(
        !glue.contains("struct Level("),
        "a plain enum gets no wrapper class"
    );
}

#[test]
fn a_ruby_async_method_is_a_gap_with_no_runtime_story() {
    let set = emit_set_with(BindKind::Ruby, &ruby_opts());
    assert!(!set.files["ext/acme_core/src/lib.rs"].contains("fn refresh"));
    assert!(
        set.gaps
            .iter()
            .any(|g| g.contains("no Ruby runtime story yet"))
    );
}

#[test]
fn a_ruby_receiver_never_takes_self_by_mutable_reference() {
    let glue = &emit_set_with(BindKind::Ruby, &ruby_opts()).files["ext/acme_core/src/lib.rs"];
    assert!(
        !glue.contains("&mut self") && !glue.contains("rb_self: &mut Self"),
        "magnus's TryConvert is implemented only for &T: {glue}"
    );
    assert!(glue.contains("fn bump(ruby: &::magnus::Ruby, rb_self: &Self, by: i64)"));
}

#[test]
fn a_ruby_receiver_borrows_fallibly_instead_of_panicking() {
    let glue = &emit_set_with(BindKind::Ruby, &ruby_opts()).files["ext/acme_core/src/lib.rs"];
    assert!(glue.contains(
        "let mut __recv = rb_self.0.try_borrow_mut().map_err(|_| ::magnus::Error::new(ruby.get_inner(&ERROR), \"already borrowed\".to_string()))?;"
    ));
    assert!(glue.contains("fn scale(ruby: &::magnus::Ruby, rb_self: &Self, factor: i64)"));
    assert!(
        !glue.contains(".0.borrow()."),
        "a getter borrows fallibly too, not through a plain borrow(): {glue}"
    );
    assert!(
        !glue.contains(".0.borrow_mut()."),
        "a setter borrows fallibly too, not through a plain borrow_mut(): {glue}"
    );
}

#[test]
fn a_ruby_writable_buffer_writes_its_mutation_back_into_the_callers_array() {
    let glue = &emit_set_with(BindKind::Ruby, &ruby_opts()).files["ext/acme_core/src/lib.rs"];
    assert!(glue.contains("fn scale_into(values: Vec<f64>, factor: f64, out: ::magnus::RArray)"));
    assert!(glue.contains("let mut out_vec = out.to_vec::<f64>()?;"));
    assert!(glue.contains(
        "out_vec.iter().enumerate().try_for_each(|(i, value)| out.store(i as isize, *value))?;"
    ));
}
