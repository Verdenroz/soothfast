//! One type, in the spelling the extendr boundary needs.
//!
//! extendr already marshals a scalar, a `&str`/`String`, and a `&[f64]`/
//! `&[u8]` sequence on its own. Past that, R has no exact spelling: no
//! 64-bit integer, no `Option` of a sequence or a handle. Those cross
//! through a hand-written `Robj` conversion instead, decided once here so
//! the glue and the generated R wrapper never disagree about which types
//! took which path.

use crate::model::Ty;

/// A scalar extendr already marshals as this Rust type, no conversion of our
/// own needed.
pub(crate) fn native_scalar(ty: &Ty) -> Option<&'static str> {
    match ty {
        Ty::Bool => Some("bool"),
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::U8 | Ty::U16 => Some("i32"),
        Ty::F32 | Ty::F64 => Some("f64"),
        _ => None,
    }
}

/// An integer with no exact R type: R's own integer is 32 bits and it has no
/// 64-bit integer at all, so this one crosses as a double, checked against
/// this Rust type on the way in.
pub(crate) fn checked_int(ty: &Ty) -> Option<&'static str> {
    match ty {
        Ty::I64 => Some("i64"),
        Ty::U64 => Some("u64"),
        Ty::U32 => Some("u32"),
        Ty::ISize => Some("isize"),
        Ty::USize => Some("usize"),
        _ => None,
    }
}

/// The element of a contiguous sequence, in the Rust type extendr already
/// marshals a numeric or raw R vector as.
pub(crate) fn buffer_native(ty: &Ty) -> Option<&'static str> {
    match ty {
        Ty::U8 => Some("u8"),
        Ty::F32 | Ty::F64 => Some("f64"),
        Ty::Bool | Ty::I8 | Ty::I16 | Ty::I32 | Ty::U16 => Some("i32"),
        _ => None,
    }
}

/// Whether an `Option<inner>` is one extendr marshals on its own. A sequence,
/// a handle, or anything else needing our own `Robj` conversion is not.
pub(crate) fn option_native(inner: &Ty) -> bool {
    native_scalar(inner).is_some() || matches!(inner, Ty::Str)
}

/// The library name every generated file agrees on: the `[lib]` name in
/// `src/rust/Cargo.toml`, the module in `extendr_module!`, and both halves of
/// `src/entrypoint.c`. R names the shared object after the package's own
/// `Package:` field, dots included, then derives the C init routine from it
/// by replacing every non-alphanumeric character with `_` (Writing R
/// Extensions §5.4); deriving the same name for the Rust side once here is
/// what keeps `R_init_<name>` and `R_init_<name>_extendr` agreeing.
pub(crate) fn lib_name(package: &str) -> String {
    let mut out = String::with_capacity(package.len());
    for c in package.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

/// Words a generated R declaration will not accept as a parameter name, plus
/// `self`, which every method wrapper already binds to the receiver.
const RESERVED: &[&str] = &[
    "if",
    "else",
    "repeat",
    "while",
    "function",
    "for",
    "next",
    "break",
    "TRUE",
    "FALSE",
    "NULL",
    "Inf",
    "NaN",
    "NA",
    "NA_integer_",
    "NA_real_",
    "NA_complex_",
    "NA_character_",
    "self",
];

/// A Rust parameter name as the R identifier it is declared under.
pub(crate) fn r_ident(name: &str) -> String {
    crate::naming::escape(name, RESERVED)
}

/// The `wrap__` symbol extendr generates for a free function.
pub(crate) fn wrap_fn(name: &str) -> String {
    format!("wrap__{name}")
}

/// The `wrap__` symbol extendr generates for a method or constructor.
pub(crate) fn wrap_method(class: &str, name: &str) -> String {
    format!("wrap__{class}__{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lib_name_replaces_every_separator_a_package_field_can_carry() {
        assert_eq!(lib_name("acme.core"), "acme_core");
        assert_eq!(lib_name("acme-core"), "acme_core");
        assert_eq!(lib_name("acmecore"), "acmecore");
    }

    #[test]
    fn every_native_scalar_is_a_type_r_already_has() {
        for ty in [Ty::Bool, Ty::I32, Ty::F64] {
            assert!(native_scalar(&ty).is_some());
        }
        assert!(native_scalar(&Ty::I64).is_none());
        assert!(native_scalar(&Ty::Str).is_none());
    }

    #[test]
    fn i64_and_friends_are_the_ones_r_has_no_exact_type_for() {
        for ty in [Ty::I64, Ty::U64, Ty::U32, Ty::ISize, Ty::USize] {
            assert!(checked_int(&ty).is_some(), "{ty:?} should be checked");
        }
        assert!(checked_int(&Ty::I32).is_none());
    }
}
