//! Emitted binding packages, pinned byte for byte.
//!
//! Regenerate with `UPDATE_GOLDENS=1 cargo test -p soothfast-bind`.

mod fixture;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use fixture::{
    cpp_opts, csharp_opts, go_opts, java_opts, kotlin_opts, lua_opts, opts, r_opts, ruby_opts, walk,
};
use soothfast_bind::{BindKind, BindOptions};

fn golden_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens")
        .join(name)
}

fn emit(kind: BindKind) -> BTreeMap<String, String> {
    emit_set(kind).files
}

fn emit_set(kind: BindKind) -> soothfast_bind::BindFileSet {
    emit_set_with(kind, &opts())
}

fn emit_set_with(kind: BindKind, opts: &BindOptions) -> soothfast_bind::BindFileSet {
    let (surface, gaps) = walk();
    kind.emit(&surface, gaps, opts).expect("emits")
}

fn emit_go() -> BTreeMap<String, String> {
    emit_go_set().files
}

fn emit_go_set() -> soothfast_bind::BindFileSet {
    emit_set_with(BindKind::Go, &go_opts())
}

fn walk_dir(dir: &Path, root: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let dotfile = matches!(name.as_str(), ".gitignore" | ".Rbuildignore");
        if (name.starts_with('.') && !dotfile) || name == "target" {
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

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("makes a directory");
    for entry in std::fs::read_dir(src).expect("reads a golden") {
        let entry = entry.expect("reads a directory entry");
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copies a golden file");
        }
    }
}

/// Stage a golden beside a copy of the fixture crate, the layout every
/// smoke test builds in: the glue crate's `Cargo.toml` depends on `acme` at
/// `path = ".."`.
fn stage_golden(lang: &str, tag: &str) -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = std::env::temp_dir().join(format!("soothfast-{tag}-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy_dir(&manifest.join("tests/fixture_crate"), &root);
    let glue = root.join("glue");
    copy_dir(&manifest.join("tests/goldens").join(lang), &glue);
    glue
}

fn check_goldens(kind: BindKind, name: &str) {
    check_goldens_with(kind, name, &opts());
}

fn check_goldens_with(kind: BindKind, name: &str, opts: &BindOptions) {
    let files = emit_set_with(kind, opts).files;
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
fn an_optional_handle_return_maps_into_its_wrapper_for_python() {
    let glue = emit(BindKind::Python)["src/lib.rs"].clone();
    assert!(glue.contains("fn find_counter(start: i64) -> Option<Counter>"));
    assert!(glue.contains("::acme::find_counter(start).map(Counter)"));
}

/// The golden's own text cannot show a type mismatch between the promised
/// return type and what the call actually hands back; only rustc can.
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
fn an_optional_handle_return_maps_into_its_wrapper_for_wasm() {
    let glue = emit(BindKind::Wasm)["src/lib.rs"].clone();
    assert!(glue.contains("pub fn find_counter(start: i64) -> Option<Counter>"));
    assert!(glue.contains("::acme::find_counter(start).map(Counter)"));
}

/// The golden's own text cannot show a type mismatch between the promised
/// return type and what the call actually hands back; only rustc can.
#[test]
fn the_wasm_golden_compiles_for_wasm32() {
    let installed = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("wasm32-unknown-unknown"))
        .unwrap_or(false);
    if !installed {
        eprintln!("wasm32-unknown-unknown not installed; skipping");
        return;
    }
    let glue = stage_golden("wasm", "wasm");
    let check = Command::new("cargo")
        .args(["check", "--target", "wasm32-unknown-unknown"])
        .current_dir(&glue)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("runs cargo check");
    assert!(check.success(), "cargo check failed for the wasm golden");
    let _ = std::fs::remove_dir_all(glue.parent().expect("has a parent"));
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
        glue.contains("ffi::slice(register_, register_len)"),
        "a parameter named after a helper must not shadow the helper"
    );
    assert!(
        files["acme_core.h"].contains("int64_t handle_, double error_, const uint8_t *register_")
    );
}

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

/// A JS-facing name is a camelCased Rust one, and both backends camelCase
/// with the same reserved-word list, so the Rust names a plan carries are a
/// faithful stand-in for what each backend actually exports.
fn export_names(plan: &soothfast_bind::plan::BindingPlan) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    for class in &plan.classes {
        names.insert(format!("class:{}", class.name));
        if let Some(ctor) = &class.ctor {
            names.insert(format!("{}::{}", class.name, ctor.name));
        }
        for accessor in &class.accessors {
            names.insert(format!("{}.{}", class.name, accessor.field));
        }
        for method in &class.methods {
            names.insert(format!("{}::{}", class.name, method.name));
        }
        for s in &class.statics {
            names.insert(format!("{}::{}", class.name, s.name));
        }
        for variant in class.variants.iter().flatten() {
            names.insert(format!("{}::{}", class.name, variant.name));
        }
    }
    for f in &plan.functions {
        names.insert(format!("fn:{}", f.name));
    }
    names
}

/// The subset of `export_names` that is `async`: wasm binds one as a
/// `Promise`, Node gaps it entirely for lack of a runtime story.
fn async_export_names(
    plan: &soothfast_bind::plan::BindingPlan,
) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    for class in &plan.classes {
        for method in class
            .methods
            .iter()
            .chain(class.statics.iter())
            .filter(|f| f.is_async)
        {
            names.insert(format!("{}::{}", class.name, method.name));
        }
    }
    for f in plan.functions.iter().filter(|f| f.is_async) {
        names.insert(format!("fn:{}", f.name));
    }
    names
}

#[test]
fn node_exports_the_same_names_wasm_does_except_async() {
    let wasm_plan = fixture::plan_for(BindKind::Wasm);
    let node_plan = fixture::plan_for(BindKind::Node);
    let wasm = export_names(&wasm_plan);
    let node = export_names(&node_plan);
    assert!(
        node.is_subset(&wasm),
        "node exports a name wasm does not: {:?}",
        node.difference(&wasm).collect::<Vec<_>>()
    );
    let missing: std::collections::BTreeSet<String> = wasm.difference(&node).cloned().collect();
    assert_eq!(missing, async_export_names(&wasm_plan));
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

#[test]
fn java_goldens() {
    check_goldens_with(BindKind::Java, "java", &java_opts());
}

#[test]
fn a_pinned_buffer_gets_one_note_for_the_whole_package() {
    let notes = emit_set_with(BindKind::Java, &java_opts()).notes;
    let pinned: Vec<&String> = notes
        .iter()
        .filter(|n| n.contains("pinned buffer blocks the collector"))
        .collect();
    assert_eq!(
        pinned.len(),
        1,
        "expected exactly one pinned note: {notes:?}"
    );
}

#[test]
fn a_failing_call_throws_the_package_exception() {
    let files = emit_set_with(BindKind::Java, &java_opts()).files;
    assert!(files.contains_key("src/main/java/acme/core/CoreException.java"));
    let exception = &files["src/main/java/acme/core/CoreException.java"];
    assert!(exception.contains("public final class CoreException extends RuntimeException"));
    let glue = &files["src/lib.rs"];
    assert!(glue.contains("env.throw_new(\"acme/core/CoreException\","));
}

#[test]
fn a_handle_class_is_autocloseable_over_a_cleaner() {
    let files = emit_set_with(BindKind::Java, &java_opts()).files;
    let natives = &files["src/main/java/acme/core/Natives.java"];
    assert!(
        natives.contains("Cleaner.create()"),
        "one shared Cleaner: {natives}"
    );
    assert!(natives.contains("System.loadLibrary(\"acme_core\");"));

    let counter = &files["src/main/java/acme/core/Counter.java"];
    assert!(counter.contains("public final class Counter implements AutoCloseable"));
    assert!(counter.contains("Natives.CLEANER.register(this, new State(ptr));"));
    assert!(counter.contains("public void close() {"));
    assert!(counter.contains("cleanable.clean();"));
}

#[test]
fn an_async_export_is_a_gap_with_no_java_runtime_story() {
    let gaps = emit_set_with(BindKind::Java, &java_opts()).gaps.join("\n");
    assert!(
        gaps.contains("no Java runtime story yet"),
        "no gap mentions it: {gaps}"
    );
    let glue = &emit_set_with(BindKind::Java, &java_opts()).files["src/lib.rs"];
    assert!(!glue.contains("nativeRefresh"));
}

#[test]
fn a_declared_constructor_is_real_and_disambiguated_by_a_marker() {
    let files = emit_set_with(BindKind::Java, &java_opts()).files;
    let counter = &files["src/main/java/acme/core/Counter.java"];
    assert!(counter.contains("static final class Raw {"));
    assert!(counter.contains("Counter(long ptr, Raw marker) {"));
    assert!(counter.contains("public Counter(long start) {"));
    assert!(counter.contains("this(nativeNew(start), Raw.INSTANCE);"));
    assert!(
        !counter.contains("new_("),
        "a real constructor needs no escaped factory name"
    );
}

#[test]
fn every_native_bearing_class_loads_the_library_before_any_call() {
    let files = emit_set_with(BindKind::Java, &java_opts()).files;
    for path in [
        "src/main/java/acme/core/Counter.java",
        "src/main/java/acme/core/Mode.java",
        "src/main/java/acme/core/Core.java",
    ] {
        assert!(
            files[path].contains("Natives.load();"),
            "{path} never forces Natives to load"
        );
    }
}

#[test]
fn a_failed_jni_call_throws_instead_of_aborting_under_panic_abort() {
    let glue = &emit_set_with(BindKind::Java, &java_opts()).files["src/lib.rs"];
    assert!(
        !glue.contains(".expect(\""),
        "panic = \"abort\" turns a bare .expect() into a JVM crash: {glue}"
    );
    assert!(glue.contains("fn __throw_unless_pending"));
    assert!(glue.contains("\"java/lang/RuntimeException\""));
    assert!(glue.matches("__throw_unless_pending(&mut env,").count() > 1);
}

#[test]
fn natives_extracts_the_bundled_library_before_falling_back_to_the_path() {
    let files = emit_set_with(BindKind::Java, &java_opts()).files;
    let natives = &files["src/main/java/acme/core/Natives.java"];
    assert!(natives.contains("getResourceAsStream(resource)"));
    assert!(natives.contains("System.load(temp.toAbsolutePath().toString());"));
    assert!(natives.contains("System.loadLibrary(\"acme_core\");"));
}

#[test]
fn kotlin_goldens() {
    check_goldens_with(BindKind::Kotlin, "kotlin", &kotlin_opts());
}

#[test]
fn the_kotlin_and_java_glue_are_byte_identical_for_the_same_plan() {
    let java = emit_set_with(BindKind::Java, &java_opts()).files;
    let kotlin = emit_set_with(BindKind::Kotlin, &kotlin_opts()).files;
    assert_eq!(
        java["src/lib.rs"], kotlin["src/lib.rs"],
        "Kotlin must reuse Java's JNI glue unchanged"
    );
}

#[test]
fn every_kotlin_external_fun_names_a_symbol_the_shared_glue_declares() {
    let files = emit_set_with(BindKind::Kotlin, &kotlin_opts()).files;
    let glue = &files["src/lib.rs"];
    let mut checked = 0;
    for (path, content) in &files {
        if !path.ends_with(".kt") {
            continue;
        }
        for line in content.lines() {
            let Some(rest) = line.trim_start().strip_prefix("private external fun ") else {
                continue;
            };
            let name = rest.split(['(', ':']).next().expect("has a name");
            assert!(
                glue.contains(&format!("_{name}<")),
                "no glue symbol matches external fun {name} declared in {path}"
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no external fun found to check");
}

#[test]
fn a_kotlin_handle_class_is_autocloseable_over_a_shared_cleaner() {
    let files = emit_set_with(BindKind::Kotlin, &kotlin_opts()).files;
    let natives = &files["src/main/kotlin/acme/core/Natives.kt"];
    assert!(
        natives.contains("Cleaner.create()"),
        "one shared Cleaner: {natives}"
    );
    assert!(natives.contains("System.loadLibrary(\"acme_core\")"));

    let counter = &files["src/main/kotlin/acme/core/Counter.kt"];
    assert!(counter.contains("class Counter private constructor(private val ptr: Long"));
    assert!(counter.contains(": AutoCloseable"));
    assert!(counter.contains("Natives.CLEANER.register(this, State(ptr))"));
    assert!(counter.contains("override fun close() {"));
    assert!(counter.contains("cleanable.clean()"));
}

#[test]
fn a_kotlin_plain_enum_mirrors_onto_an_enum_class() {
    let files = emit_set_with(BindKind::Kotlin, &kotlin_opts()).files;
    let level = &files["src/main/kotlin/acme/core/Level.kt"];
    assert!(level.contains("enum class Level {"));
    assert!(level.contains("Low,"));
    assert!(level.contains("High,"));
}

#[test]
fn a_kotlin_declared_constructor_is_a_secondary_one_disambiguated_by_a_marker() {
    let files = emit_set_with(BindKind::Kotlin, &kotlin_opts()).files;
    let counter = &files["src/main/kotlin/acme/core/Counter.kt"];
    assert!(counter.contains("private object Raw"));
    assert!(counter.contains("private constructor(private val ptr: Long, marker: Raw)"));
    assert!(counter.contains("constructor(start: Long) : this(nativeNew(start), Raw)"));
}

#[test]
fn a_kotlin_free_functions_optional_return_lines_up_with_its_own_indent() {
    let files = emit_set_with(BindKind::Kotlin, &kotlin_opts()).files;
    let module = &files["src/main/kotlin/acme/core/Core.kt"];
    assert!(module.contains(
        "fun findCounter(start: Long): Counter? {\n    val ptr_ = nativeFindCounter(start)\n    return if (ptr_ == 0L) null else Counter(ptr_, Counter.Raw)\n}"
    ));
}

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
fn a_ruby_writable_buffer_writes_its_mutation_back_into_the_callers_array() {
    let glue = &emit_set_with(BindKind::Ruby, &ruby_opts()).files["ext/acme_core/src/lib.rs"];
    assert!(glue.contains("fn scale_into(values: Vec<f64>, factor: f64, out: ::magnus::RArray)"));
    assert!(glue.contains("let mut out_vec = out.to_vec::<f64>()?;"));
    assert!(glue.contains(
        "out_vec.iter().enumerate().try_for_each(|(i, value)| out.store(i as isize, *value))?;"
    ));
}

fn emit_cpp() -> BTreeMap<String, String> {
    emit_cpp_set().files
}

fn emit_cpp_set() -> soothfast_bind::BindFileSet {
    emit_set_with(BindKind::Cpp, &cpp_opts())
}

#[test]
fn cpp_goldens() {
    check_goldens_with(BindKind::Cpp, "cpp", &cpp_opts());
}

#[test]
fn the_cpp_module_name_is_the_namespace_paths_last_element() {
    let files = emit_cpp();
    for path in [
        "Cargo.toml",
        "README.md",
        "src/lib.rs",
        "core.h",
        "core.pc",
        "core.hpp",
    ] {
        assert!(files.contains_key(path), "missing {path}");
    }
    let manifest = &files["Cargo.toml"];
    assert!(manifest.contains("name = \"core-cabi\""));
    let header = &files["core.hpp"];
    assert!(header.contains("namespace acme::core {"));
    assert!(header.contains("#include \"core.h\""));
    let readme = &files["README.md"];
    assert!(readme.contains("## C++"));
}

#[test]
fn a_handle_owns_its_pointer_through_a_deleter_calling_the_c_free() {
    let header = emit_cpp()["core.hpp"].clone();
    assert!(header.contains("class Counter {"));
    assert!(header.contains(
        "struct Deleter {\n        void operator()(core_counter *p) const noexcept { core_counter_free(p); }\n    };"
    ));
    assert!(header.contains("std::unique_ptr<core_counter, Deleter> handle_;"));
    assert!(
        header.contains("explicit Counter(int64_t start) : handle_(core_counter_new(start)) {}")
    );
}

#[test]
fn a_failing_call_throws_after_freeing_the_c_message() {
    let header = emit_cpp()["core.hpp"].clone();
    assert!(header.contains("class Error : public std::runtime_error {"));
    assert!(header.contains("char *error = nullptr;"));
    assert!(header.contains("std::string message(error);"));
    assert!(header.contains("core_string_free(error);"));
    assert!(header.contains("throw Error(message);"));
}

#[test]
fn a_plain_enum_crosses_as_an_enum_class_cast_to_the_c_enum() {
    let header = emit_cpp()["core.hpp"].clone();
    assert!(header.contains("enum class Level {\n    Low,\n    High,\n};"));
    assert!(header.contains("int64_t at(Level level) const {"));
    assert!(header.contains("static_cast<core_level>(level)"));
}

#[test]
fn a_buffer_crosses_as_a_span_both_borrowed_and_mutable() {
    let header = emit_cpp()["core.hpp"].clone();
    assert!(header.contains("std::vector<uint8_t> digest(std::span<const uint8_t> data)"));
    assert!(header.contains("data.data(), data.size()"));
    assert!(header.contains(
        "void scale_into(std::span<const double> values, double factor, std::span<double> out)"
    ));
}

#[test]
fn a_returned_sequence_is_copied_into_a_vector_and_freed() {
    let header = emit_cpp()["core.hpp"].clone();
    assert!(header.contains("inline std::vector<double> to_vector_f64(core_f64_array arr) {"));
    assert!(header.contains("core_f64_array_free(arr);"));
    assert!(header.contains("return to_vector_f64(raw_result);"));
}

#[test]
fn a_string_is_taken_as_a_view_and_returned_as_an_owned_string() {
    let header = emit_cpp()["core.hpp"].clone();
    assert!(header.contains("inline std::string take_string(char *raw) {"));
    assert!(header.contains("std::string greet(std::string_view name) {"));
    assert!(header.contains("std::string name_owned(name);"));
    assert!(header.contains("name_owned.c_str()"));
}

#[test]
fn a_parameter_named_after_a_cpp_keyword_is_escaped() {
    let header = emit_cpp()["core.hpp"].clone();
    assert!(header.contains(
        "uint64_t stamp(int64_t handle, double error_, std::span<const uint8_t> register_) {"
    ));
}

#[test]
fn what_c_cannot_spell_is_also_unsupported_for_cpp() {
    let set = emit_cpp_set();
    let gaps = set.gaps.join("\n");
    for expected in ["async fn", "HashMap", "Option<"] {
        assert!(
            gaps.contains(expected),
            "no gap mentions {expected}: {gaps}"
        );
    }
    assert!(!set.files["core.hpp"].contains("index_all"));
}

/// The emitted header must parse and type-check clean under both compilers
/// the repo has installed, at the warning level a real consumer builds
/// with. `-fsyntax-only` needs no library to link against: the header's own
/// declarations, pulled in from `core.h`, are enough. The driver file lives
/// in a scratch directory, not the golden one: `cpp_goldens` walks that
/// directory and would trip over an extra file a concurrent test left
/// behind.
#[test]
fn the_cpp_header_compiles_clean_under_available_compilers() {
    use std::process::Command;

    let dir = golden_dir("cpp");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock is after epoch")
        .as_nanos();
    let scratch = std::env::temp_dir().join(format!(
        "soothfast-bind-cpp-check-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&scratch).expect("makes scratch dir");
    let check = scratch.join("check.cpp");
    std::fs::write(&check, "#include \"core.hpp\"\n").expect("writes check.cpp");

    let mut checked = 0;
    for compiler in ["g++", "clang++"] {
        let args = ["-std=c++20", "-Wall", "-Wextra", "-Werror", "-fsyntax-only"];
        let output = Command::new(compiler)
            .args(args)
            .arg("-I")
            .arg(&dir)
            .arg(&check)
            .output();
        let Ok(output) = output else {
            eprintln!("{compiler} not on PATH; skipping");
            continue;
        };
        checked += 1;
        assert!(
            output.status.success(),
            "{compiler} {} -I {} {} exited with {}\nstderr:\n{}",
            args.join(" "),
            dir.display(),
            check.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
    if checked == 0 {
        eprintln!("neither g++ nor clang++ on PATH; skipping");
    }
}

#[test]
fn lua_goldens() {
    check_goldens_with(BindKind::Lua, "lua", &lua_opts());
}

#[test]
fn the_lua_module_lands_at_the_require_paths_own_directory() {
    let files = emit_set_with(BindKind::Lua, &lua_opts()).files;
    assert!(files.contains_key("acme/core.lua"), "{:?}", files.keys());
    assert!(files.contains_key("core.h"));
    assert!(files.contains_key("core.pc"));
}

#[test]
fn the_cdef_block_declares_the_same_symbols_the_header_would() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    for symbol in [
        "typedef struct core_counter core_counter;",
        "core_counter * core_counter_new(int64_t start);",
        "void core_counter_free(core_counter *handle);",
        "int64_t core_counter_bump(const core_counter *handle, int64_t by, char **error);",
    ] {
        assert!(lua.contains(symbol), "cdef omits {symbol}");
    }
    assert!(
        !lua.contains("#include"),
        "ffi.cdef cannot read a preprocessor directive"
    );
    assert!(
        !lua.contains("extern \"C\""),
        "ffi.cdef cannot read a C++ linkage block"
    );
}

#[test]
fn a_handle_is_freed_through_ffi_gc_and_an_idempotent_close() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("ffi.gc(ptr, lib.core_counter_free)"));
    assert!(lua.contains("function Counter:close()"));
    assert!(lua.contains("if self.ptr == nil then"));
    assert!(lua.contains("ffi.gc(self.ptr, nil)"));
    assert!(lua.contains("lib.core_counter_free(self.ptr)"));
}

#[test]
fn a_failing_call_raises_after_freeing_the_message() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("local err = ffi.new(\"char*[1]\")"));
    assert!(lua.contains("error(lua_string(err[0]))"));
    assert!(lua.contains("lib.core_string_free(s)"));
}

#[test]
fn a_lua_plain_enum_crosses_as_a_validated_string_not_an_ordinal() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("local LEVEL_TO_C = { low = 0, high = 1 }"));
    assert!(lua.contains("invalid Level: "));
    assert!(
        !lua.contains("local Level = {}"),
        "a plain enum gets no wrapper class"
    );
}

#[test]
fn a_buffer_parameter_accepts_a_matching_cdata_array_without_copying() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("ffi.istype(\"double[?]\", value)"));
    assert!(lua.contains("return value, ffi.sizeof(value) / ffi.sizeof(\"double\")"));
}

#[test]
fn a_writable_buffer_writes_back_into_a_plain_table_only() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("if type(out) == \"table\" then"));
    assert!(lua.contains("out[i] = out_ptr[i - 1]"));
}

#[test]
fn a_lua_buffer_call_copies_with_no_transfer_notes() {
    let notes = emit_set_with(BindKind::Lua, &lua_opts()).notes;
    assert!(
        notes
            .iter()
            .all(|n| !n.contains("would arrive without a copy")
                && !n.contains("allocates a fresh sequence")),
        "lua copies every buffer regardless of the signature unless the \
         caller already holds a matching FFI array: {notes:?}"
    );
}

#[test]
fn a_returned_array_is_a_metatyped_cdata_not_a_copied_table() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("ffi.metatype(\"core_f64_array\", {"));
    assert!(lua.contains("__len = function(self) return tonumber(self.len) end,"));
    assert!(lua.contains("if key == \"totable\" then"));
    assert!(lua.contains(
        "if key % 1 ~= 0 or key < 1 or key > tonumber(self.len) then\n\t\t\t\terror(\"core_f64_array index out of range: \" .. tostring(key))"
    ));
    assert!(lua.contains("return ffi.gc(ret, lib.core_f64_array_free)"));
    assert!(
        !lua.contains("_to_table"),
        "the array-to-table copier is gone: returns cross as cdata"
    );
}

#[test]
fn a_returned_array_can_be_closed_like_a_handle() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("local function core_f64_array_close(self)"));
    assert!(lua.contains("if self.data == nil then"));
    assert!(lua.contains("ffi.gc(self, nil)\n\tlib.core_f64_array_free(self)"));
    assert!(lua.contains("self.len = 0\n\tself.data = nil"));
    assert!(lua.contains("if key == \"close\" then\n\t\t\treturn core_f64_array_close"));
}

#[test]
fn a_buffer_argument_accepts_a_returned_array_with_no_copy() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("if ffi.istype(\"core_f64_array\", value) then"));
    assert!(lua.contains("return value.data, tonumber(value.len)"));
    assert!(
        !lua.contains("core_i64_array"),
        "i64 is only ever a buffer element here, never a return: naming an \
         undeclared array struct in ffi.istype would be a parse error"
    );
}

#[test]
fn a_parameter_named_after_luas_own_raise_builtin_is_escaped() {
    let lua = &emit_set_with(BindKind::Lua, &lua_opts()).files["acme/core.lua"];
    assert!(lua.contains("function M.stamp(handle, error_, register)"));
}

#[test]
fn an_async_method_is_a_gap_with_no_lua_runtime_story() {
    let set = emit_set_with(BindKind::Lua, &lua_opts());
    assert!(!set.files["acme/core.lua"].contains("refresh"));
    assert!(
        set.gaps
            .iter()
            .any(|g| g.contains("no Lua runtime story yet"))
    );
}

#[test]
fn csharp_goldens() {
    check_goldens_with(BindKind::CSharp, "csharp", &csharp_opts());
}

#[test]
fn a_csharp_handle_is_released_through_a_safehandle() {
    let files = emit_set_with(BindKind::CSharp, &csharp_opts()).files;
    let counter = &files["Counter.cs"];
    assert!(counter.contains("private sealed class Handle : SafeHandleZeroOrMinusOneIsInvalid"));
    assert!(counter.contains("protected override bool ReleaseHandle()"));
    assert!(counter.contains("Native.acme_core_counter_free(handle);"));
    assert!(counter.contains("public sealed class Counter : IDisposable"));
    assert!(counter.contains("public void Dispose()"));
}

#[test]
fn a_failing_csharp_call_throws_and_frees_the_message() {
    let files = emit_set_with(BindKind::CSharp, &csharp_opts()).files;
    assert!(files.contains_key("CoreException.cs"));
    let exception = &files["CoreException.cs"];
    assert!(exception.contains("public sealed class CoreException : Exception"));
    assert!(exception.contains("Native.acme_core_string_free(error);"));
    let counter = &files["Counter.cs"];
    assert!(counter.contains("if (error != IntPtr.Zero)"));
    assert!(counter.contains("throw CoreException.FromNative(error);"));
}

#[test]
fn a_csharp_buffer_parameter_is_pinned_with_fixed() {
    let module = &emit_set_with(BindKind::CSharp, &csharp_opts()).files["Core.cs"];
    assert!(
        module.contains(
            "public static double[] Normalize(ReadOnlySpan<double> input, double factor)"
        )
    );
    assert!(module.contains("fixed (double* inputPtr = input)"));
    assert!(module.contains("Marshal.Copy(result.Data, value, 0, (int)result.Len);"));
    assert!(module.contains("Native.acme_core_f64_array_free(result);"));
}

#[test]
fn a_csharp_plain_enum_crosses_via_explicit_casts() {
    let files = emit_set_with(BindKind::CSharp, &csharp_opts()).files;
    let level = &files["Level.cs"];
    assert!(level.contains("public enum Level"));
    assert!(level.contains("    Low,\n"));
    assert!(level.contains("    High,\n"));
    let counter = &files["Counter.cs"];
    assert!(counter.contains("public long At(Level level)"));
    assert!(counter.contains("(int)level"));
    let native = &files["Native.cs"];
    assert!(
        native.contains(
            "internal static extern long acme_core_counter_at(IntPtr handle, int level);"
        )
    );
}

#[test]
fn an_async_csharp_method_is_a_gap_with_no_dotnet_runtime_story() {
    let set = emit_set_with(BindKind::CSharp, &csharp_opts());
    assert!(
        set.gaps
            .iter()
            .any(|g| g.contains("no .NET async story yet"))
    );
    assert!(!set.files["Counter.cs"].contains("Refresh"));
}

#[test]
fn a_pinned_csharp_buffer_gets_one_note_for_the_whole_package() {
    let notes = emit_set_with(BindKind::CSharp, &csharp_opts()).notes;
    let pinned: Vec<&String> = notes
        .iter()
        .filter(|n| n.contains("pinned buffer blocks the collector"))
        .collect();
    assert_eq!(
        pinned.len(),
        1,
        "expected exactly one pinned note: {notes:?}"
    );
}

#[test]
fn what_c_cannot_spell_is_also_unsupported_for_csharp() {
    let set = emit_set_with(BindKind::CSharp, &csharp_opts());
    let gaps = set.gaps.join("\n");
    for expected in ["HashMap", "Option<"] {
        assert!(
            gaps.contains(expected),
            "no gap mentions {expected}: {gaps}"
        );
    }
}

#[test]
fn the_csharp_native_declarations_match_the_c_header_symbols() {
    let c_files = emit(BindKind::CAbi);
    let header = &c_files["acme_core.h"];
    let native = &emit_set_with(BindKind::CSharp, &csharp_opts()).files["Native.cs"];
    for symbol in [
        "acme_core_counter_new",
        "acme_core_counter_bump",
        "acme_core_counter_free",
        "acme_core_digest",
        "acme_core_normalize",
        "acme_core_string_free",
    ] {
        assert!(header.contains(symbol), "header omits {symbol}");
        assert!(native.contains(symbol), "Native.cs omits {symbol}");
    }
}

#[test]
fn an_optional_string_binds_across_the_c_family() {
    for kind in [
        BindKind::CAbi,
        BindKind::Go,
        BindKind::Java,
        BindKind::Kotlin,
        BindKind::Cpp,
        BindKind::Lua,
        BindKind::CSharp,
    ] {
        let opts = match kind {
            BindKind::Go => go_opts(),
            BindKind::Java => java_opts(),
            BindKind::Kotlin => kotlin_opts(),
            BindKind::Cpp => cpp_opts(),
            BindKind::Lua => lua_opts(),
            BindKind::CSharp => csharp_opts(),
            _ => opts(),
        };
        let set = emit_set_with(kind, &opts);
        assert!(
            !set.gaps.iter().any(|g| g.contains("describe")),
            "{kind:?} gaps describe: {:?}",
            set.gaps
        );
    }
}

#[test]
fn an_optional_scalar_is_a_gap_across_the_c_family() {
    for kind in [
        BindKind::CAbi,
        BindKind::Go,
        BindKind::Java,
        BindKind::Kotlin,
        BindKind::Cpp,
        BindKind::Lua,
        BindKind::CSharp,
    ] {
        let opts = match kind {
            BindKind::Go => go_opts(),
            BindKind::Java => java_opts(),
            BindKind::Kotlin => kotlin_opts(),
            BindKind::Cpp => cpp_opts(),
            BindKind::Lua => lua_opts(),
            BindKind::CSharp => csharp_opts(),
            _ => opts(),
        };
        let set = emit_set_with(kind, &opts);
        assert!(
            set.gaps.iter().any(|g| g.contains("maybe_ratio")),
            "{kind:?} does not gap maybe_ratio: {:?}",
            set.gaps
        );
        assert!(
            !set.files
                .iter()
                .any(|(name, f)| name != "README.md" && f.contains("maybe_ratio")),
            "{kind:?} emitted a symbol for maybe_ratio"
        );
    }
}

#[test]
fn an_optional_scalar_binds_natively_outside_the_c_family() {
    for kind in [
        BindKind::Python,
        BindKind::Wasm,
        BindKind::Node,
        BindKind::Ruby,
        BindKind::R,
    ] {
        let opts = match kind {
            BindKind::Ruby => ruby_opts(),
            BindKind::R => r_opts(),
            _ => opts(),
        };
        let set = emit_set_with(kind, &opts);
        assert!(
            !set.gaps.iter().any(|g| g.contains("maybe_ratio")),
            "{kind:?} gaps maybe_ratio: {:?}",
            set.gaps
        );
    }
}
