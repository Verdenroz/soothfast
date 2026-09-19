use std::collections::BTreeSet;

use crate::BindKind;
use crate::model::Ty;

/// Why a language cannot carry this type, if it cannot.
pub(super) fn unsupported(kind: BindKind, ty: &Ty, mirrored: &BTreeSet<String>) -> Option<String> {
    if kind != BindKind::Python && mentions_text(ty) {
        return Some(
            "a [bind.types] mapping crosses into Python only for now; other backends report it"
                .into(),
        );
    }
    // Go, C++, Lua and C# all call the same C functions, so each inherits
    // the restriction; Java and Kotlin have no generic container either,
    // and no more of a story than C does for a sequence of anything but one
    // primitive.
    if matches!(
        kind,
        BindKind::CAbi
            | BindKind::Go
            | BindKind::Java
            | BindKind::Kotlin
            | BindKind::Cpp
            | BindKind::Lua
            | BindKind::CSharp
    ) && let Some(why) = unsupported_by_c(ty, mirrored)
    {
        return Some(why);
    }
    if kind == BindKind::R
        && let Some(why) = unsupported_by_r(ty)
    {
        return Some(why);
    }
    match ty {
        Ty::Map(..) if kind == BindKind::Wasm => Some(
            "wasm-bindgen carries no map type; return a list of pairs, or a \
             struct with named fields"
                .into(),
        ),
        Ty::Map(..) if kind == BindKind::Node => Some(
            "napi-rs carries no map type; return a list of pairs, or a \
             struct with named fields"
                .into(),
        ),
        Ty::Tuple(_) if kind == BindKind::Wasm => Some(
            "wasm-bindgen carries no tuple type; return a struct with named \
             fields"
                .into(),
        ),
        Ty::Tuple(_) if kind == BindKind::Node => {
            Some("napi-rs carries no tuple type; return a struct with named fields".into())
        }
        // wasm-bindgen has no ABI for a boolean array: it is neither a
        // numeric typed array nor a string.
        Ty::List(inner) if **inner == Ty::Bool && kind == BindKind::Wasm => {
            Some("wasm-bindgen carries no `bool` array; return a list of `u8` instead".into())
        }
        Ty::List(inner) | Ty::Optional(inner) => unsupported(kind, inner, mirrored),
        Ty::Map(key, value) => {
            unsupported(kind, key, mirrored).or_else(|| unsupported(kind, value, mirrored))
        }
        Ty::Tuple(items) => items.iter().find_map(|t| unsupported(kind, t, mirrored)),
        _ => None,
    }
}

fn mentions_text(ty: &Ty) -> bool {
    match ty {
        Ty::Text(_) => true,
        Ty::List(inner) | Ty::Optional(inner) => mentions_text(inner),
        Ty::Map(key, value) => mentions_text(key) || mentions_text(value),
        Ty::Tuple(items) => items.iter().any(mentions_text),
        _ => false,
    }
}

/// Why C cannot carry this type, if it cannot.
///
/// C has no generic container, so anything that is not a scalar, a string, a
/// handle or a contiguous run of one primitive would need a bespoke struct
/// and a bespoke way to release it. Those are reported rather than guessed.
fn unsupported_by_c(ty: &Ty, mirrored: &BTreeSet<String>) -> Option<String> {
    match ty {
        Ty::Map(..) => Some(
            "C has no map type; return a sequence of pairs, or an exported \
             type with accessors"
                .into(),
        ),
        Ty::Tuple(_) => {
            Some("C has no tuple type; return an exported type with named fields".into())
        }
        Ty::List(inner) if !inner.is_primitive() => Some(format!(
            "a sequence of `{}` has no C spelling that owns its elements; a \
             sequence of one primitive crosses as a pointer and a length",
            inner.render()
        )),
        // A mirrored enum crosses by value, with no pointer to hand back as
        // null; only a handle or a string has a null-pointer spelling for
        // "absent".
        Ty::Optional(inner) if matches!(**inner, Ty::Class(ref name) if mirrored.contains(name)) => {
            Some(format!(
                "`Option<{}>` has no C spelling for a mirrored enum, which \
                 crosses by value; take it as a return only",
                inner.render()
            ))
        }
        Ty::Optional(inner) if !matches!(**inner, Ty::Class(_) | Ty::Str) => Some(format!(
            "`Option<{}>` has no C spelling; only an optional exported type \
             or string does, as a pointer that may be null",
            inner.render()
        )),
        _ => None,
    }
}

/// Why R cannot carry this type, if it cannot.
///
/// R has no map or tuple type either, and a sequence of anything but one
/// primitive has no vector to become. Unlike C, an `Option` of anything the
/// plan can otherwise carry is not restricted here for a return or a field:
/// it crosses as `NULL` by hand rather than needing a type of its own. A
/// parameter is narrower; see [`super::optional_scalar_param_is_ready`].
fn unsupported_by_r(ty: &Ty) -> Option<String> {
    match ty {
        Ty::Map(..) => Some(
            "R has no map type; return a sequence of pairs, or an exported \
             type with accessors"
                .into(),
        ),
        Ty::Tuple(_) => {
            Some("R has no tuple type; return an exported type with named fields".into())
        }
        Ty::List(inner) if !inner.is_primitive() => Some(format!(
            "a sequence of `{}` has no R vector to become; a sequence of one \
             primitive crosses as a numeric or raw vector",
            inner.render()
        )),
        // R's logical vector holds extendr's own tri-state Rbool (TRUE,
        // FALSE, or NA), not a plain bool; extendr's derive marshals it as
        // one, never as `Vec<bool>`/`&[bool]`.
        Ty::List(inner) if **inner == Ty::Bool => Some(
            "a `bool` sequence has no R vector to become: R's own logical \
             vector holds a tri-state NA-or-boolean, not a plain bool"
                .into(),
        ),
        _ => None,
    }
}

/// Whether a `None`/`NULL` parameter of this shape has a hand-built `Robj`
/// conversion on the way in: a string already did; an `f64`, an `f64`
/// sequence and a mirrored enum are the ones [`crate::extendr`] adds. Any
/// other optional parameter is reported rather than guessed; the return and
/// field directions have no such restriction (see [`unsupported_by_r`]).
pub(super) fn optional_scalar_param_is_ready(inner: &Ty, mirrored: &BTreeSet<String>) -> bool {
    match inner {
        Ty::Str | Ty::F64 => true,
        Ty::Class(name) => mirrored.contains(name),
        Ty::List(elem) => **elem == Ty::F64,
        _ => false,
    }
}
