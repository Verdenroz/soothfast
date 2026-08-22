//! One type, in both spellings it needs.
//!
//! Every crossing type is written twice: once as C reads it in the header,
//! once as Rust writes it in the `extern "C"` shim. Deciding both in one
//! place is what keeps the two files describing the same ABI.

use crate::model::Ty;
use crate::plan::BindingPlan;

/// How one type is spelled on each side of the boundary.
pub(crate) struct Spelling {
    pub c: String,
    pub rust: String,
}

fn both(c: &str, rust: &str) -> Spelling {
    Spelling {
        c: c.into(),
        rust: rust.into(),
    }
}

/// A scalar's spelling, or `None` for a type that is not one.
///
/// `bool` is included: the C `_Bool` and Rust `bool` agree on one byte, which
/// is the guarantee `#[repr(C)]` rests on.
pub(crate) fn scalar(ty: &Ty) -> Option<Spelling> {
    let pair = match ty {
        Ty::Bool => ("bool", "bool"),
        Ty::I8 => ("int8_t", "i8"),
        Ty::I16 => ("int16_t", "i16"),
        Ty::I32 => ("int32_t", "i32"),
        Ty::I64 => ("int64_t", "i64"),
        Ty::ISize => ("ptrdiff_t", "isize"),
        Ty::U8 => ("uint8_t", "u8"),
        Ty::U16 => ("uint16_t", "u16"),
        Ty::U32 => ("uint32_t", "u32"),
        Ty::U64 => ("uint64_t", "u64"),
        Ty::USize => ("size_t", "usize"),
        Ty::F32 => ("float", "f32"),
        Ty::F64 => ("double", "f64"),
        _ => return None,
    };
    Some(both(pair.0, pair.1))
}

/// The element of a contiguous sequence this backend carries as a pointer
/// and a length, or `None` for one it does not.
pub(crate) fn element(ty: &Ty) -> Option<Spelling> {
    match ty {
        Ty::Bytes => scalar(&Ty::U8),
        Ty::List(inner) => scalar(inner),
        _ => None,
    }
}

/// The value a failing call returns before the caller has looked at `error`.
///
/// C has no `Result`, so a failure still has to hand back something of the
/// right type. Every one of these is the zero the caller must not read.
pub(crate) fn zero(ty: &Ty, plan: &BindingPlan, module: &str) -> String {
    match ty {
        Ty::Unit => String::new(),
        Ty::Bool => "false".into(),
        Ty::F32 | Ty::F64 => "0.0".into(),
        Ty::Class(name) if plan.is_mirrored(name) => {
            format!("{}::default()", handle_rust(name, module))
        }
        Ty::Str | Ty::Class(_) | Ty::Optional(_) => "::std::ptr::null_mut()".into(),
        Ty::Bytes | Ty::List(_) => format!("{}::empty()", array_rust(ty, module)),
        _ => "0".into(),
    }
}

/// The C name of the struct a returned sequence comes back in.
pub(crate) fn array_c(ty: &Ty, module: &str) -> String {
    let element = element(ty).map(|s| s.rust).unwrap_or_default();
    format!("{module}_{element}_array")
}

/// The same struct, as the glue crate names it.
pub(crate) fn array_rust(ty: &Ty, module: &str) -> String {
    pascal(&array_c(ty, module))
}

/// The C name of an exported type's opaque handle.
pub(crate) fn handle_c(name: &str, module: &str) -> String {
    format!("{module}_{}", snake(name))
}

/// The same handle, as the glue crate names it.
pub(crate) fn handle_rust(name: &str, module: &str) -> String {
    pascal(&handle_c(name, module))
}

/// A snake_case C name as a Rust type name: `acme_core_counter` becomes
/// `AcmeCoreCounter`.
pub(crate) fn pascal(name: &str) -> String {
    name.split('_')
        .filter(|p| !p.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect()
}

/// A Rust name as the lowercase word a C identifier is built from.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_c_name_and_its_rust_twin_come_from_the_same_words() {
        assert_eq!(handle_c("Counter", "acme_core"), "acme_core_counter");
        assert_eq!(handle_rust("Counter", "acme_core"), "AcmeCoreCounter");
        assert_eq!(snake("HTTPServer"), "httpserver");
        assert_eq!(snake("BumpBy"), "bump_by");
    }

    #[test]
    fn every_scalar_is_spelled_on_both_sides() {
        for ty in [Ty::Bool, Ty::I64, Ty::USize, Ty::F32, Ty::F64] {
            let s = scalar(&ty).expect("scalar");
            assert!(!s.c.is_empty() && !s.rust.is_empty());
        }
        assert!(scalar(&Ty::Str).is_none());
    }
}
