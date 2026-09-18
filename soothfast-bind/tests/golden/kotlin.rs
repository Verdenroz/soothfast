use soothfast_bind::BindKind;

use crate::fixture::{java_opts, kotlin_opts};
use crate::{check_goldens_with, emit_set_with};

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
