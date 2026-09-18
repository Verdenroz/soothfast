use std::process::Command;

use soothfast_bind::BindKind;

use crate::{check_goldens, emit, stage_golden};

#[test]
fn wasm_goldens() {
    check_goldens(BindKind::Wasm, "wasm");
}

#[test]
fn a_surface_with_nothing_async_carries_no_runtime() {
    let glue = emit(BindKind::Wasm)["src/lib.rs"].clone();
    assert!(!glue.contains("OnRuntime"));
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
        wasm.contains("impl ::std::convert::From<BindErrorString> for ::wasm_bindgen::JsError")
    );
    assert!(wasm.contains("-> Result<i64, JsError>"));
}

#[test]
fn a_failing_call_rejects_with_a_real_error_not_a_bare_string() {
    let wasm = emit(BindKind::Wasm)["src/lib.rs"].clone();
    assert!(
        wasm.contains("::wasm_bindgen::JsError::new(&::std::string::ToString::to_string(&err.0))"),
        "a bare JsValue::from_str leaves catch (e) {{ e.message }} undefined in JavaScript: {wasm}"
    );
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

// The golden's own text cannot show a type mismatch between the promised
// return type and what the call actually hands back; only rustc can.
#[test]
fn the_wasm_golden_compiles_for_wasm32() {
    let installed = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("wasm32-unknown-unknown"))
        .unwrap_or(false);
    if !crate::support::require_toolchain(
        installed,
        "wasm32-unknown-unknown target",
        "rustup target add wasm32-unknown-unknown",
    ) {
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
