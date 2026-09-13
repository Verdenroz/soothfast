//! One type, in its P/Invoke spelling and the public C# spelling built
//! around it.
//!
//! The C side is never re-derived: struct and symbol names come straight
//! from [`crate::cabi::types`], so a C# declaration can never drift from the
//! header P/Invoke calls against.

use crate::cabi::types as c;
use crate::model::Ty;
use crate::plan::BindingPlan;

/// A scalar's blittable P/Invoke spelling and its public C# spelling.
///
/// The two differ only for `bool`: C's `_Bool` is one byte, and marshalling
/// a C# `bool` across `[DllImport]` without `[MarshalAs(UnmanagedType.U1)]`
/// picks a 4-byte `BOOL` on Windows, so the extern declaration takes `byte`
/// and the wrapper converts at the boundary instead.
pub(crate) struct Spelling {
    pub native: &'static str,
    pub public: &'static str,
}

fn both(spelling: &'static str) -> Spelling {
    Spelling {
        native: spelling,
        public: spelling,
    }
}

/// A scalar's spelling, or `None` for a type that is not one.
pub(crate) fn scalar(ty: &Ty) -> Option<Spelling> {
    match ty {
        Ty::Bool => Some(Spelling {
            native: "byte",
            public: "bool",
        }),
        Ty::I8 => Some(both("sbyte")),
        Ty::I16 => Some(both("short")),
        Ty::I32 => Some(both("int")),
        Ty::I64 => Some(both("long")),
        Ty::ISize => Some(both("nint")),
        Ty::U8 => Some(both("byte")),
        Ty::U16 => Some(both("ushort")),
        Ty::U32 => Some(both("uint")),
        Ty::U64 => Some(both("ulong")),
        Ty::USize => Some(both("nuint")),
        Ty::F32 => Some(both("float")),
        Ty::F64 => Some(both("double")),
        _ => None,
    }
}

/// The element of a contiguous sequence this backend carries pinned, or
/// `None` for one it does not.
pub(crate) fn element(ty: &Ty) -> Option<Spelling> {
    match ty {
        Ty::Bytes => scalar(&Ty::U8),
        Ty::List(inner) => scalar(inner),
        _ => None,
    }
}

/// A parameter or return's P/Invoke type for everything that is not a
/// buffer or a string, which the caller builds from two arguments or a
/// pinned pointer instead: a scalar, a handle (`IntPtr`, or `int` for a
/// mirrored enum crossing by value), or an owned sequence's array struct,
/// named exactly as [`crate::cabi::types::array_rust`] names it in the
/// embedded C crate.
pub(crate) fn native_ty(ty: &Ty, plan: &BindingPlan, module: &str) -> String {
    match ty {
        Ty::Unit => "void".into(),
        Ty::Str => "IntPtr".into(),
        Ty::Class(name) if plan.is_mirrored(name) => "int".into(),
        Ty::Class(_) => "IntPtr".into(),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(_) => "IntPtr".into(),
            Ty::Str => "IntPtr".into(),
            _ => "void".into(),
        },
        ty if c::element(ty).is_some() => c::array_rust(ty, module),
        ty => scalar(ty).map(|s| s.native.to_string()).unwrap_or_default(),
    }
}

/// The public API's type for the same value: a scalar, `string`, a handle
/// class by name, or a managed array.
pub(crate) fn public_ty(ty: &Ty) -> String {
    match ty {
        Ty::Unit => "void".into(),
        Ty::Str => "string".into(),
        Ty::Class(name) => name.clone(),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) => name.clone(),
            Ty::Str => "string?".into(),
            other => public_ty(other),
        },
        ty if ty.is_primitive() => scalar(ty).expect("checked").public.into(),
        ty => element(ty)
            .map(|e| format!("{}[]", e.public))
            .unwrap_or_default(),
    }
}

/// A buffer parameter's public type: a view over the caller's own memory
/// rather than a copy, mutable only when the signature borrows it that way.
pub(crate) fn buffer_public_ty(element: &Ty, writable: bool) -> String {
    let public = scalar(element).expect("buffer element is a scalar").public;
    match writable {
        true => format!("Span<{public}>"),
        false => format!("ReadOnlySpan<{public}>"),
    }
}

/// The native library name: the whole dotted namespace, snake_cased, e.g.
/// `Acme.Core` -> `acme_core`. Matches how [`crate::jni::types::native_lib_name`]
/// treats a dotted Java package, and how [`crate::cgo::types::package_name`]
/// treats a slash-free Go module path: both snake_case the whole string when
/// there is no further hierarchy to shorten it to.
pub(crate) fn lib_name(package: &str) -> String {
    c::snake(package)
}

/// The class holding every free function, named after the namespace's last
/// segment, the same way [`crate::jni::types::module_class`] names Java's.
pub(crate) fn module_class(package: &str) -> String {
    let last = package.rsplit('.').next().unwrap_or(package);
    c::pascal(&c::snake(last))
}

/// The one exception type the package throws, named after its module class
/// rather than per Rust error type: every fallible call raises the same
/// shape regardless of what it returns on success.
pub(crate) fn exception_class(module_class: &str) -> String {
    format!("{module_class}Exception")
}

/// The .NET runtime identifier a target triple's native library stages
/// under, e.g. `runtimes/linux-x64/native/`. Distinct from
/// [`crate::jni::native_dir_for_triple`]: RIDs are `<os>-<arch>` in .NET's
/// own vocabulary (`osx`, `win`, `arm64`), not Java's.
pub(crate) fn rid_for_triple(triple: &str) -> String {
    let arch = if triple.starts_with("aarch64") {
        "arm64"
    } else {
        "x64"
    };
    let os = if triple.contains("apple-darwin") {
        "osx"
    } else if triple.contains("windows") {
        "win"
    } else {
        "linux"
    };
    format!("{os}-{arch}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lib_name_covers_the_whole_namespace_but_the_module_class_does_not() {
        assert_eq!(lib_name("Acme.Core"), "acme_core");
        assert_eq!(module_class("Acme.Core"), "Core");
    }

    #[test]
    fn every_mainstream_triple_maps_to_a_dotnet_runtime_identifier() {
        assert_eq!(rid_for_triple("x86_64-unknown-linux-gnu"), "linux-x64");
        assert_eq!(rid_for_triple("aarch64-unknown-linux-gnu"), "linux-arm64");
        assert_eq!(rid_for_triple("x86_64-apple-darwin"), "osx-x64");
        assert_eq!(rid_for_triple("aarch64-apple-darwin"), "osx-arm64");
        assert_eq!(rid_for_triple("x86_64-pc-windows-msvc"), "win-x64");
    }

    #[test]
    fn bool_is_the_one_scalar_whose_native_and_public_spellings_differ() {
        let s = scalar(&Ty::Bool).expect("scalar");
        assert_eq!(s.native, "byte");
        assert_eq!(s.public, "bool");
        assert!(scalar(&Ty::Str).is_none());
    }
}
