//! A `[bind.types]` `"str"` mapping in the Python glue: the foreign type
//! crosses as `str`, parsed on the way in through `FromStr` and rendered on
//! the way out through `Display`.

use std::fmt::Write;

use crate::model::{Ownership, Ty};
use crate::plan::Function;

use super::py_ident;

const MAP_ERR: &str =
    "|e| ::pyo3::exceptions::PyValueError::new_err(::std::string::ToString::to_string(&e))";

/// Whether the type is, or holds, a mapped foreign type.
pub(crate) fn mentions(ty: &Ty) -> bool {
    match ty {
        Ty::Text(_) => true,
        Ty::List(inner) | Ty::Optional(inner) => mentions(inner),
        Ty::Map(key, value) => mentions(key) || mentions(value),
        Ty::Tuple(items) => items.iter().any(mentions),
        _ => false,
    }
}

/// One `let` per parameter holding a mapped type, rebinding the string the
/// call received to the parsed value. They run ahead of the call itself, and
/// ahead of any detach, so a `?` never lands inside the detached closure.
pub(crate) fn preludes(function: &Function, indent: &str) -> String {
    let mut out = String::new();
    for param in &function.params {
        let Some(parsed) = parse(&py_ident(&param.name), &param.ty) else {
            continue;
        };
        let binding = match param.ownership == Ownership::BorrowedMut {
            true => format!("mut {}", py_ident(&param.name)),
            false => py_ident(&param.name),
        };
        let _ = writeln!(out, "{indent}let {binding} = {parsed};");
    }
    out
}

/// A string, or an option or sequence of one, parsed into the mapped type.
/// The target type is left to inference from the call the value reaches.
pub(crate) fn parse(expr: &str, ty: &Ty) -> Option<String> {
    Some(match ty {
        Ty::Text(_) => format!("{expr}.parse().map_err({MAP_ERR})?"),
        Ty::Optional(inner) if matches!(**inner, Ty::Text(_)) => {
            format!("{expr}.map(|s| s.parse().map_err({MAP_ERR})).transpose()?")
        }
        Ty::List(inner) if matches!(**inner, Ty::Text(_)) => format!(
            "{expr}.into_iter().map(|s| s.parse().map_err({MAP_ERR})).collect::<Result<Vec<_>, ::pyo3::PyErr>>()?"
        ),
        _ => return None,
    })
}

/// A mapped value, or an option or sequence of one, rendered to the string
/// Python receives.
pub(crate) fn out(expr: &str, ty: &Ty) -> Option<String> {
    Some(match ty {
        Ty::Text(_) => format!("::std::string::ToString::to_string(&{expr})"),
        Ty::Optional(inner) if matches!(**inner, Ty::Text(_)) => {
            format!("{expr}.map(|v| ::std::string::ToString::to_string(&v))")
        }
        Ty::List(inner) if matches!(**inner, Ty::Text(_)) => {
            format!("{expr}.into_iter().map(|v| ::std::string::ToString::to_string(&v)).collect()")
        }
        _ => return None,
    })
}
