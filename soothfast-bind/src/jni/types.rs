//! One type, in every spelling the JNI boundary needs.
//!
//! A crossing primitive is spelled four times: the Java declaration, the
//! Kotlin declaration, the `jni` crate's array family (`JDoubleArray`,
//! `jdoubleArray`, `get_double_array_region`), and the raw Rust type the glue
//! reads it as. Deciding all four here is what keeps the Java source, the
//! Kotlin source, and the Rust glue from disagreeing about the same value.
//! Kotlin escapes a reserved-word identifier the same way Java does, with a
//! trailing underscore (see [`super::kotlin_ident`]).

use crate::model::Ty;
use crate::naming;
use crate::plan::BindingPlan;

/// The private native method a public call named `name` goes through.
///
/// Built straight off the Rust name rather than the escaped public one: a
/// Rust ctor named `new` becomes the public `new_` (Java reserves `new`),
/// but `nativeNew` collides with nothing and never needed the escape.
pub(crate) fn native_method_name(name: &str) -> String {
    format!("native{}", pascal(&naming::camel(name)))
}

/// How one scalar is spelled on each side of the boundary.
pub(crate) struct Spelling {
    pub java: &'static str,
    pub kotlin: &'static str,
    pub rust: &'static str,
}

fn spelling(java: &'static str, kotlin: &'static str, rust: &'static str) -> Spelling {
    Spelling { java, kotlin, rust }
}

/// A scalar's spelling, or `None` for a type that is not one.
///
/// Java has no unsigned integer type, so each unsigned width reinterprets
/// the same bits as its signed twin, the same choice `&[u8]` already makes
/// crossing as `byte[]`.
pub(crate) fn scalar(ty: &Ty) -> Option<Spelling> {
    let triple = match ty {
        Ty::Bool => ("boolean", "Boolean", "bool"),
        Ty::I8 | Ty::U8 => ("byte", "Byte", "i8"),
        Ty::I16 | Ty::U16 => ("short", "Short", "i16"),
        Ty::I32 | Ty::U32 => ("int", "Int", "i32"),
        Ty::I64 | Ty::U64 | Ty::ISize | Ty::USize => ("long", "Long", "i64"),
        Ty::F32 => ("float", "Float", "f32"),
        Ty::F64 => ("double", "Double", "f64"),
        _ => return None,
    };
    Some(spelling(triple.0, triple.1, triple.2))
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

/// The Java type of one value with no exported-type involved: a scalar, a
/// string, or a sequence of one primitive. Handle and enum types are named
/// directly after the class, decided by the caller, since both a mirrored
/// enum and an opaque handle share the same Java-facing spelling.
pub(crate) fn java_ty(ty: &Ty) -> String {
    match ty {
        Ty::Unit => "void".into(),
        Ty::Str => "String".into(),
        Ty::Class(name) => name.clone(),
        Ty::Optional(inner) => java_ty(inner),
        ty if ty.is_primitive() => scalar(ty).expect("checked").java.into(),
        ty => element(ty)
            .map(|e| format!("{}[]", e.java))
            .unwrap_or_default(),
    }
}

/// The `jni` wrapper type Rust reads a sequence through, e.g. `JDoubleArray`.
pub(crate) fn array_class(java_element: &str) -> String {
    format!("J{}Array", pascal(java_element))
}

/// The raw `jni::sys` alias for the same array, e.g. `jdoubleArray`.
pub(crate) fn array_sys(java_element: &str) -> String {
    format!("j{java_element}Array")
}

/// A parameter's native type: the high-level `jni` wrapper for a string or a
/// sequence, since the glue reads it through one either way; a mirrored
/// enum crosses as its ordinal, everything else as itself.
pub(crate) fn native_param_ty(ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Str => "::jni::objects::JString<'local>".into(),
        Ty::Class(name) if plan.is_mirrored(name) => "i32".into(),
        Ty::Class(_) => "i64".into(),
        ty if ty.is_primitive() => scalar(ty).expect("checked").rust.into(),
        ty => element(ty)
            .map(|e| format!("::jni::objects::{}<'local>", array_class(e.java)))
            .unwrap_or_default(),
    }
}

/// A returned value's native type: the raw `jni::sys` alias, since a native
/// method hands the JVM a bare value rather than a wrapper.
pub(crate) fn native_return_ty(ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Unit => String::new(),
        Ty::Str => "::jni::sys::jstring".into(),
        Ty::Class(name) if plan.is_mirrored(name) => "i32".into(),
        Ty::Class(_) => "i64".into(),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(_) => "i64".into(),
            other => native_return_ty(other, plan),
        },
        ty if ty.is_primitive() => scalar(ty).expect("checked").rust.into(),
        ty => element(ty)
            .map(|e| format!("::jni::sys::{}", array_sys(e.java)))
            .unwrap_or_default(),
    }
}

/// A parameter or return's type as the Java *native declaration* spells it:
/// a handle crosses as `long`, a mirrored enum as `int`, everything else the
/// same as its public-facing type.
pub(crate) fn native_java_ty(ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Class(name) if plan.is_mirrored(name) => "int".into(),
        Ty::Class(_) => "long".into(),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(_) => "long".into(),
            other => native_java_ty(other, plan),
        },
        _ => java_ty(ty),
    }
}

/// The Kotlin type of one value with no exported-type involved, the Kotlin
/// twin of [`java_ty`]. `Option<Class>` alone crosses as a nullable class
/// type; every other shape reaching here has already been proven bindable
/// without one, so it stays non-null.
pub(crate) fn kotlin_ty(ty: &Ty) -> String {
    match ty {
        Ty::Unit => "Unit".into(),
        Ty::Str => "String".into(),
        Ty::Class(name) => name.clone(),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) => format!("{name}?"),
            other => kotlin_ty(other),
        },
        ty if ty.is_primitive() => scalar(ty).expect("checked").kotlin.into(),
        ty => element(ty)
            .map(|e| format!("{}Array", e.kotlin))
            .unwrap_or_default(),
    }
}

/// A parameter or return's type as the Kotlin *native declaration* spells
/// it, the Kotlin twin of [`native_java_ty`].
pub(crate) fn native_kotlin_ty(ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Class(name) if plan.is_mirrored(name) => "Int".into(),
        Ty::Class(_) => "Long".into(),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(_) => "Long".into(),
            other => native_kotlin_ty(other, plan),
        },
        _ => kotlin_ty(ty),
    }
}

/// Capitalizes just the first letter, for a name that already has its word
/// boundaries marked by case (turning `camelCase` into `PascalCase`).
pub(crate) fn pascal(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// `PascalCase`s a snake_case name by capitalizing every underscore-
/// separated word, for a name with no case of its own to preserve.
fn words_pascal(name: &str) -> String {
    name.split('_')
        .filter(|w| !w.is_empty())
        .map(pascal)
        .collect()
}

/// A Rust name, snake_cased for a native library name.
pub(crate) fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let mut prev_lower = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            if prev_lower && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower = false;
        } else if c.is_ascii_alphanumeric() {
            prev_lower = true;
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
            prev_lower = false;
        }
    }
    out.trim_matches('_').to_string()
}

/// The native library name: the whole dotted Java package, snake_cased,
/// e.g. `io.soothfast.stats` -> `io_soothfast_stats`. The last segment
/// alone (`stats`) is common enough on a library path to collide with
/// something else already there.
pub(crate) fn native_lib_name(java_package: &str) -> String {
    snake(java_package)
}

/// The `<Module>` class holding every free function, named after the
/// package's last segment.
pub(crate) fn module_class(java_package: &str) -> String {
    let last = java_package.rsplit('.').next().unwrap_or(java_package);
    words_pascal(&snake(last))
}

/// The `extern "system"` symbol the JVM looks up for one native method.
///
/// The package separator `.` becomes a plain `_`; a literal `_` already
/// inside a segment has to escape to `_1` first, since `_` is the very
/// character used to join them.
pub(crate) fn jni_symbol(java_package: &str, class: &str, method: &str) -> String {
    let mut out = String::from("Java_");
    for part in java_package.split('.') {
        out.push_str(&mangle(part));
        out.push('_');
    }
    out.push_str(&mangle(class));
    out.push('_');
    out.push_str(&mangle(method));
    out
}

fn mangle(segment: &str) -> String {
    segment.replace('_', "_1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scalar_reinterprets_its_unsigned_twin() {
        assert_eq!(scalar(&Ty::U8).expect("scalar").java, "byte");
        assert_eq!(scalar(&Ty::I8).expect("scalar").java, "byte");
        assert_eq!(scalar(&Ty::U64).expect("scalar").rust, "i64");
        assert!(scalar(&Ty::Str).is_none());
    }

    #[test]
    fn the_native_lib_name_covers_the_whole_package_but_the_module_class_does_not() {
        assert_eq!(native_lib_name("io.soothfast.stats"), "io_soothfast_stats");
        assert_eq!(module_class("io.soothfast.stats"), "Stats");
    }

    #[test]
    fn a_literal_underscore_escapes_before_the_package_joins_on_one() {
        assert_eq!(
            jni_symbol("io.soothfast.stats", "Summary", "nativeBumpAll"),
            "Java_io_soothfast_stats_Summary_nativeBumpAll"
        );
        assert_eq!(
            jni_symbol("acme", "My_Class", "go"),
            "Java_acme_My_1Class_go"
        );
    }
}
