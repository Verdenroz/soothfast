//! Emitted binding packages, pinned byte for byte.
//!
//! Regenerate with `UPDATE_GOLDENS=1 cargo test -p soothfast-bind`.

mod fixture;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fixture::{opts, walk};
use soothfast_bind::BindKind;

fn golden_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens")
        .join(name)
}

fn emit(kind: BindKind) -> BTreeMap<String, String> {
    emit_set(kind).files
}

fn emit_set(kind: BindKind) -> soothfast_bind::BindFileSet {
    let (surface, gaps) = walk();
    kind.emit(&surface, gaps, &opts()).expect("emits")
}

fn walk_dir(dir: &Path, root: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if (name.starts_with('.') && name != ".gitignore") || name == "target" {
            continue;
        }
        if path.is_dir() {
            walk_dir(&path, root, out);
        } else if let Ok(content) = std::fs::read_to_string(&path) {
            let rel = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, content);
        }
    }
}

fn check_goldens(kind: BindKind, name: &str) {
    let files = emit(kind);
    let dir = golden_dir(name);

    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        let _ = std::fs::remove_dir_all(&dir);
        for (rel, content) in &files {
            let target = dir.join(rel);
            std::fs::create_dir_all(target.parent().expect("has a parent")).expect("makes dirs");
            std::fs::write(&target, content).expect("writes");
        }
        return;
    }

    let mut expected = BTreeMap::new();
    walk_dir(&dir, &dir, &mut expected);
    assert!(
        !expected.is_empty(),
        "no {name} goldens found; run UPDATE_GOLDENS=1 cargo test -p soothfast-bind"
    );
    let got: Vec<&String> = files.keys().collect();
    let want: Vec<&String> = expected.keys().collect();
    assert_eq!(got, want, "file set changed");
    for (rel, content) in &files {
        assert_eq!(content, &expected[rel], "content of {rel} changed");
    }
}

#[test]
fn python_goldens() {
    check_goldens(BindKind::Python, "python");
}

#[test]
fn wasm_goldens() {
    check_goldens(BindKind::Wasm, "wasm");
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
fn a_surface_with_nothing_async_carries_no_runtime() {
    let glue = emit(BindKind::Wasm)["src/lib.rs"].clone();
    assert!(!glue.contains("OnRuntime"));
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

#[test]
fn one_rust_surface_reaches_both_languages_as_the_same_class() {
    let python = emit(BindKind::Python)["src/lib.rs"].clone();
    let wasm = emit(BindKind::Wasm)["src/lib.rs"].clone();
    for glue in [&python, &wasm] {
        assert!(glue.contains("pub struct Counter(::acme::Counter);"));
        assert!(glue.contains("fn normalize("));
    }
    assert!(python.contains("#[pyclass(name = \"Counter\")]"));
    assert!(wasm.contains("#[wasm_bindgen]\npub struct Counter"));
}

#[test]
fn javascript_spellings_are_renamed_and_rust_names_are_left_alone() {
    let wasm = emit(BindKind::Wasm)["src/lib.rs"].clone();
    assert!(wasm.contains("#[wasm_bindgen(js_name = bumpAll)]"));
    assert!(wasm.contains("pub fn bump_all("));
    assert!(!wasm.contains("js_name = digest"), "digest needs no rename");
}

#[test]
fn a_failing_call_rejects_through_a_local_newtype() {
    let wasm = emit(BindKind::Wasm)["src/lib.rs"].clone();
    assert!(
        wasm.contains("impl ::std::convert::From<BindErrorString> for ::wasm_bindgen::JsValue")
    );
    assert!(wasm.contains("-> Result<i64, JsValue>"));
}

#[test]
fn the_wasm_manifest_names_no_targets() {
    let manifest = emit(BindKind::Wasm)["Cargo.toml"].clone();
    assert!(manifest.contains("crate-type = [\"cdylib\", \"rlib\"]"));
    assert!(manifest.contains("wasm-bindgen = \"0.2\""));
    assert!(!manifest.contains("target"), "one .wasm runs everywhere");
}

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
        glue.contains("ffi::slice(register_, register__len)"),
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
