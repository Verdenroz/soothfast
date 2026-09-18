use std::process::Command;

use soothfast_bind::BindKind;

use crate::fixture::{opts, walk};
use crate::{check_goldens, emit, emit_set, stage_golden};

#[test]
fn python_goldens() {
    check_goldens(BindKind::Python, "python");
}

#[test]
fn emission_is_deterministic() {
    assert_eq!(emit(BindKind::Python), emit(BindKind::Python));
}

#[test]
fn the_glue_names_every_exported_item_exactly_once() {
    let files = emit(BindKind::Python);
    let glue = &files["src/lib.rs"];
    for name in ["Counter", "Mode", "normalize", "digest", "index_all"] {
        assert!(glue.contains(name), "glue omits {name}");
    }
    assert_eq!(glue.matches("#[pymodule]").count(), 1);
    assert_eq!(glue.matches("pub struct Counter(").count(), 1);
}

#[test]
fn a_failing_call_raises_through_a_local_newtype() {
    let glue = emit(BindKind::Python)["src/lib.rs"].clone();
    assert!(glue.contains("struct BindErrorString(::std::string::String);"));
    assert!(glue.contains("impl ::std::convert::From<BindErrorString> for ::pyo3::PyErr"));
    assert!(glue.contains(".map_err(BindErrorString)?"));
    assert!(
        !glue.contains("for ::pyo3::PyErr {\n    fn from(err: ::std"),
        "an impl over the user's own error type would violate the orphan rule"
    );
}

#[test]
fn a_sequence_of_one_primitive_crosses_through_a_buffer_both_ways() {
    let glue = emit(BindKind::Python)["src/lib.rs"].clone();
    assert!(
        glue.contains("fn normalize(py: Python<'_>, input: BorrowedF64, factor: f64) -> F64Array")
    );
    assert!(glue.contains("F64Array::new(out)"));
    assert!(glue.contains("unsafe fn __getbuffer__"));
    assert!(glue.contains("m.add_class::<F64Array>()?;"));
}

#[test]
fn bytes_stay_bytes_rather_than_becoming_an_array_class() {
    let glue = emit(BindKind::Python)["src/lib.rs"].clone();
    assert!(glue.contains("fn digest(py: Python<'_>, data: BorrowedU8) -> Vec<u8>"));
    assert!(!glue.contains("U8Array"));
}

#[test]
fn a_returned_sequence_is_reported_only_where_a_buffer_saves_a_copy() {
    let python = emit_set(BindKind::Python).notes.join("\n");
    assert!(python.contains("normalize: returning `Vec<f64>` allocates"));

    let wasm = emit_set(BindKind::Wasm).notes.join("\n");
    assert!(
        !wasm.contains("returning `Vec<f64>`"),
        "wasm copies a mutable slice in as well as out, so an out-parameter \
         buys it nothing"
    );
}

#[test]
fn a_buffer_call_runs_with_the_lock_released() {
    let glue = emit(BindKind::Python)["src/lib.rs"].clone();
    assert!(glue.contains("py.detach(|| ::acme::digest(data.as_slice()))"));
    assert!(glue.contains("fn bump_all(&self, py: Python<'_>, by: BorrowedI64) -> i64"));
    assert!(glue.contains("py.detach(|| self.0.bump_all(by.into_vec()))"));
}

#[test]
fn a_scalar_call_keeps_the_lock_rather_than_paying_to_drop_it() {
    let glue = emit(BindKind::Python)["src/lib.rs"].clone();
    assert!(glue.contains("fn bump(&self, by: i64) -> PyResult<i64>"));
    assert!(glue.contains("fn at(&self, level: Level) -> i64"));
}

#[test]
fn an_optional_handle_return_maps_into_its_wrapper_for_python() {
    let glue = emit(BindKind::Python)["src/lib.rs"].clone();
    assert!(glue.contains("fn find_counter(start: i64) -> Option<Counter>"));
    assert!(glue.contains("::acme::find_counter(start).map(Counter)"));
}

// The golden's own text cannot show a type mismatch between the promised
// return type and what the call actually hands back; only rustc can.
#[test]
fn the_python_golden_compiles_against_a_real_python() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("python3 not on PATH; skipping");
        return;
    }
    let glue = stage_golden("python", "python");
    let check = Command::new("cargo")
        .arg("check")
        .current_dir(&glue)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("runs cargo check");
    assert!(check.success(), "cargo check failed for the python golden");
    let _ = std::fs::remove_dir_all(glue.parent().expect("has a parent"));
}

#[test]
fn a_type_rustdoc_never_proved_sync_keeps_the_lock() {
    let (mut surface, gaps) = walk();
    for ty in &mut surface.types {
        ty.sync = false;
    }
    let glue = BindKind::Python
        .emit(&surface, gaps, &opts())
        .expect("emits")
        .files["src/lib.rs"]
        .clone();
    assert!(
        !glue.contains("fn bump_all(&self, py: Python<'_>"),
        "a method may not hand `&self` to another thread on an unproven guess"
    );
    assert!(
        glue.contains("py.detach(|| ::acme::digest"),
        "a free function borrows no receiver, so it is unaffected"
    );
}

#[test]
fn an_async_call_is_awaited_inside_a_runtime() {
    let glue = emit(BindKind::Python)["src/lib.rs"].clone();
    assert!(glue.contains("async fn refresh(&self) -> u32 {"));
    assert!(glue.contains("OnRuntime(self.0.refresh()).await"));
    assert!(glue.contains("fn runtime() -> &'static ::tokio::runtime::Runtime"));
}

#[test]
fn the_manifest_asks_for_async_support_only_when_the_surface_needs_it() {
    let manifest = emit(BindKind::Python)["Cargo.toml"].clone();
    assert!(manifest.contains("\"experimental-async\""));
    assert!(manifest.contains("tokio = { version = \"1\""));
    assert!(manifest.contains("crate-type = [\"cdylib\"]"));
    assert!(manifest.contains("acme = { path = \"..\" }"));
}

#[test]
fn an_enum_carrying_data_stays_a_handle_and_says_so() {
    let (surface, gaps) = walk();
    let emitted = BindKind::Python
        .emit(&surface, gaps, &opts())
        .expect("emits");
    assert!(emitted.files["src/lib.rs"].contains("pub struct Mode(::acme::Mode);"));
    assert!(
        emitted
            .notes
            .iter()
            .any(|n| n.starts_with("Mode: an enum carrying data"))
    );
}
