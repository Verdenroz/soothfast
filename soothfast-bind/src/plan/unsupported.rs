use crate::BindKind;
use crate::model::Ty;

/// Why a language cannot carry this type, if it cannot.
pub(super) fn unsupported(kind: BindKind, ty: &Ty) -> Option<String> {
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
    ) && let Some(why) = unsupported_by_c(ty)
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
        Ty::List(inner) | Ty::Optional(inner) => unsupported(kind, inner),
        Ty::Map(key, value) => unsupported(kind, key).or_else(|| unsupported(kind, value)),
        Ty::Tuple(items) => items.iter().find_map(|t| unsupported(kind, t)),
        _ => None,
    }
}

/// Why C cannot carry this type, if it cannot.
///
/// C has no generic container, so anything that is not a scalar, a string, a
/// handle or a contiguous run of one primitive would need a bespoke struct
/// and a bespoke way to release it. Those are reported rather than guessed.
fn unsupported_by_c(ty: &Ty) -> Option<String> {
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
/// plan can otherwise carry is not restricted here: it crosses as `NULL` by
/// hand rather than needing a type of its own.
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
        _ => None,
    }
}
