use std::collections::BTreeMap;

use soothfast_bind::BindKind;

use crate::fixture::cpp_opts;
use crate::{check_goldens_with, emit_set_with, golden_dir};

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

// `-fsyntax-only` needs no library to link. The driver lives in a scratch
// directory, not the golden one, since `cpp_goldens` walks that directory.
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
    let compiler_available = ["g++", "clang++"]
        .iter()
        .any(|c| Command::new(c).arg("--version").output().is_ok());
    if !crate::support::require_toolchain(
        compiler_available,
        "g++ or clang++",
        "install a C++ compiler",
    ) {
        return;
    }

    std::fs::create_dir_all(&scratch).expect("makes scratch dir");
    let check = scratch.join("check.cpp");
    std::fs::write(&check, "#include \"core.hpp\"\n").expect("writes check.cpp");

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
}
