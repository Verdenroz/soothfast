use soothfast_bind::BindKind;

use crate::fixture::csharp_opts;
use crate::{check_goldens_with, emit, emit_set_with};

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
