use soothfast_bind::BindKind;

use crate::fixture::java_opts;
use crate::{check_goldens_with, emit_set_with};

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
fn a_second_writable_buffer_reads_and_writes_back_without_staying_pinned() {
    let glue = &emit_set_with(BindKind::Java, &java_opts()).files["src/lib.rs"];
    assert!(glue.contains("nativeSplit"));
    assert!(glue.contains("get_double_array_region(&hi, 0, &mut __raw)"));
    assert!(glue.contains("let mut hi_buf: Vec<f64> = __raw;"));
    assert!(glue.contains("set_double_array_region(&hi, 0, &hi_out)"));
}

#[test]
fn natives_extracts_the_bundled_library_before_falling_back_to_the_path() {
    let files = emit_set_with(BindKind::Java, &java_opts()).files;
    let natives = &files["src/main/java/acme/core/Natives.java"];
    assert!(natives.contains("getResourceAsStream(resource)"));
    assert!(natives.contains("System.load(temp.toAbsolutePath().toString());"));
    assert!(natives.contains("System.loadLibrary(\"acme_core\");"));
}
