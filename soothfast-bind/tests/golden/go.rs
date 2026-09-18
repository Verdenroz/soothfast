use std::collections::BTreeMap;
use std::process::Command;

use soothfast_bind::BindKind;

use crate::fixture::go_opts;
use crate::{check_goldens_with, emit_set_with, golden_dir};

fn emit_go() -> BTreeMap<String, String> {
    emit_go_set().files
}

fn emit_go_set() -> soothfast_bind::BindFileSet {
    emit_set_with(BindKind::Go, &go_opts())
}

#[test]
fn go_goldens() {
    check_goldens_with(BindKind::Go, "go", &go_opts());
}

#[test]
fn the_go_package_name_is_the_module_paths_last_element() {
    let files = emit_go();
    for path in ["Cargo.toml", "README.md", "src/lib.rs", "core.h", "core.pc"] {
        assert!(files.contains_key(path), "missing {path}");
    }
    let manifest = &files["Cargo.toml"];
    assert!(manifest.contains("name = \"core-cabi\""));
    let go_mod = &files["go.mod"];
    assert!(go_mod.contains("module github.com/acme/core"));
    let glue = &files["core.go"];
    assert!(glue.contains("package core"));
    let readme = &files["README.md"];
    assert!(readme.contains("## Go"));
    assert!(readme.contains("untested on Windows"));
}

#[test]
fn a_handle_is_closed_through_its_free_and_backstopped_by_a_finalizer() {
    let glue = emit_go()["core.go"].clone();
    assert!(glue.contains("type Counter struct {\n\tptr *C.core_counter\n}"));
    assert!(glue.contains("func (recv *Counter) Close() error {"));
    assert!(glue.contains("if recv.ptr == nil {\n\t\treturn nil\n\t}"));
    assert!(glue.contains("C.core_counter_free(recv.ptr)"));
    assert!(glue.contains("runtime.SetFinalizer(h, (*Counter).Close)"));
}

#[test]
fn a_failing_call_becomes_a_go_error() {
    let glue = emit_go()["core.go"].clone();
    assert!(glue.contains("func (recv *Counter) Bump(by int64) (int64, error) {"));
    assert!(glue.contains("var errPtr *C.char"));
    assert!(glue.contains("err := errors.New(goString(errPtr))"));
    assert!(glue.contains("return 0, err"));
}

#[test]
fn a_parameter_named_after_a_go_builtin_is_escaped() {
    let glue = emit_go()["core.go"].clone();
    assert!(glue.contains("func Stamp(handle int64, error_ float64, register []byte) uint64 {"));
}

#[test]
fn an_async_fn_is_a_gap_with_no_go_runtime_story() {
    let set = emit_go_set();
    assert!(
        set.gaps
            .iter()
            .any(|g| g.contains("no Go runtime story yet")),
        "gaps: {:?}",
        set.gaps
    );
    assert!(!set.files["core.go"].contains("Refresh"));
}

#[test]
fn a_buffer_parameter_passes_the_backing_array_with_an_empty_slice_guard() {
    let glue = emit_go()["core.go"].clone();
    assert!(glue.contains("func Digest(data []byte) []byte {"));
    assert!(glue.contains("(*C.uint8_t)(unsafe.Pointer(bufPtr(data))), C.size_t(len(data))"));
    assert!(glue.contains("func bufPtr[T any](s []T) *T {"));
}

#[test]
fn a_returned_sequence_is_copied_into_a_slice_and_freed() {
    let glue = emit_go()["core.go"].clone();
    assert!(glue.contains("func float64Slice(arr C.core_f64_array) []float64 {"));
    assert!(glue.contains("defer C.core_f64_array_free(arr)"));
    assert!(glue.contains("return float64Slice(C.core_normalize("));
}

#[test]
fn a_plain_enum_mirrors_onto_a_typed_int_via_a_shared_c_conversion() {
    let glue = emit_go()["core.go"].clone();
    assert!(glue.contains("type Level int32"));
    assert!(glue.contains("LevelLow  Level = 0"));
    assert!(glue.contains("LevelHigh Level = 1"));
    assert!(glue.contains("func (recv Level) c() C.core_level {"));
    assert!(glue.contains("C.core_counter_at(recv.ptr, level.c())"));
}

#[test]
fn an_optional_handle_return_wraps_a_nullable_pointer_for_go() {
    let glue = emit_go()["core.go"].clone();
    assert!(glue.contains("func FindCounter(start int64) *Counter {"));
    assert!(glue.contains("if p := C.core_find_counter(C.int64_t(start)); p != nil {"));
    assert!(glue.contains("return wrapCounter(p)"));
}

#[test]
fn the_emitted_go_is_already_gofmt_clean() {
    let dir = golden_dir("go");
    let Ok(output) = Command::new("gofmt").arg("-l").arg(&dir).output() else {
        eprintln!("gofmt not on PATH; skipping");
        return;
    };
    let dirty = String::from_utf8_lossy(&output.stdout);
    assert!(dirty.trim().is_empty(), "gofmt would reformat: {dirty}");
}

#[test]
fn a_method_call_after_close_panics_instead_of_dereferencing_a_freed_pointer() {
    let glue = emit_go()["core.go"].clone();
    assert!(glue.contains(
        "func (recv *Counter) Value() int64 {\n\tif recv.ptr == nil {\n\t\tpanic(\"Counter is closed\")\n\t}\n"
    ));
}

#[test]
fn what_c_cannot_spell_is_also_unsupported_for_go() {
    let set = emit_go_set();
    let gaps = set.gaps.join("\n");
    for expected in ["HashMap", "Option<"] {
        assert!(
            gaps.contains(expected),
            "no gap mentions {expected}: {gaps}"
        );
    }
}
