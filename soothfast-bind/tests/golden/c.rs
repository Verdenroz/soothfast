use soothfast_bind::BindKind;

use crate::{check_goldens, emit, emit_set};

#[test]
fn c_goldens() {
    check_goldens(BindKind::CAbi, "c");
}

#[test]
fn the_header_and_the_glue_declare_the_same_symbols() {
    let files = emit(BindKind::CAbi);
    let header = &files["acme_core.h"];
    let glue = &files["src/lib.rs"];
    for symbol in [
        "acme_core_counter_new",
        "acme_core_counter_bump",
        "acme_core_counter_bump_all",
        "acme_core_counter_at",
        "acme_core_counter_value",
        "acme_core_counter_free",
        "acme_core_digest",
        "acme_core_normalize",
        "acme_core_f64_array_free",
        "acme_core_string_free",
    ] {
        assert!(header.contains(symbol), "header omits {symbol}");
        assert!(glue.contains(symbol), "glue omits {symbol}");
    }
}

#[test]
fn an_exported_type_is_an_opaque_pointer_the_caller_releases() {
    let files = emit(BindKind::CAbi);
    assert!(files["acme_core.h"].contains("typedef struct acme_core_counter acme_core_counter;"));
    assert!(
        files["acme_core.h"].contains("void acme_core_counter_free(acme_core_counter *handle);")
    );
    assert!(files["src/lib.rs"].contains("pub struct AcmeCoreCounter(::acme::Counter);"));
}

#[test]
fn a_failing_call_writes_its_message_through_an_out_parameter() {
    let files = emit(BindKind::CAbi);
    assert!(files["acme_core.h"].contains(
        "int64_t acme_core_counter_bump(const acme_core_counter *handle, int64_t by, char **error);"
    ));
    assert!(files["src/lib.rs"].contains("unsafe { ffi::report(error, &reason) };"));
}

#[test]
fn a_sequence_crosses_as_a_pointer_and_a_length() {
    let files = emit(BindKind::CAbi);
    let header = &files["acme_core.h"];
    assert!(header.contains("const int64_t *by, size_t by_len"));
    assert!(header.contains("acme_core_f64_array acme_core_normalize("));
    assert!(header.contains("void acme_core_f64_array_free(acme_core_f64_array array);"));
}

#[test]
fn a_payload_free_enum_mirrors_onto_a_c_enumeration() {
    let files = emit(BindKind::CAbi);
    assert!(files["acme_core.h"].contains("ACME_CORE_LEVEL_LOW = 0,"));
    assert!(files["acme_core.h"].contains("acme_core_level level"));
    assert!(!files["acme_core.h"].contains("acme_core_level *level"));
}

#[test]
fn a_parameter_cannot_collide_with_what_the_backend_generates_around_it() {
    let files = emit(BindKind::CAbi);
    let glue = &files["src/lib.rs"];
    assert!(glue.contains("acme_core_stamp(handle_: i64, error_: f64, register_: *const u8"));
    assert!(
        glue.contains("ffi::slice(register_, register_len)"),
        "a parameter named after a helper must not shadow the helper"
    );
    assert!(
        files["acme_core.h"].contains("int64_t handle_, double error_, const uint8_t *register_")
    );
}

#[test]
fn what_c_cannot_spell_is_reported_rather_than_guessed() {
    let set = emit_set(BindKind::CAbi);
    let gaps = set.gaps.join("\n");
    for expected in ["async fn", "HashMap", "Option<"] {
        assert!(
            gaps.contains(expected),
            "no gap mentions {expected}: {gaps}"
        );
    }
    let glue = &set.files["src/lib.rs"];
    assert!(!glue.contains("acme_core_index_all"));
    assert!(!glue.contains("acme_core_trim"));
}
