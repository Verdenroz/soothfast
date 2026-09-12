//! One type, in its Go spelling and the C spelling cgo compiles against.
//!
//! The C side is never re-derived: it comes straight from
//! [`crate::cabi::types`], so a Go signature can never drift from the header
//! cgo includes.

use crate::cabi::types as c;
use crate::model::Ty;

/// A scalar's Go spelling and the `C.<name>` cast that reaches it.
pub(crate) struct Spelling {
    pub go: &'static str,
    pub cgo: String,
}

/// A scalar's spelling, or `None` for a type that is not one.
pub(crate) fn scalar(ty: &Ty) -> Option<Spelling> {
    let go = match ty {
        Ty::Bool => "bool",
        Ty::I8 => "int8",
        Ty::I16 => "int16",
        Ty::I32 => "int32",
        Ty::I64 => "int64",
        Ty::ISize => "int",
        Ty::U8 => "uint8",
        Ty::U16 => "uint16",
        Ty::U32 => "uint32",
        Ty::U64 => "uint64",
        Ty::USize => "uint",
        Ty::F32 => "float32",
        Ty::F64 => "float64",
        _ => return None,
    };
    Some(Spelling {
        go,
        cgo: format!("C.{}", c::scalar(ty)?.c),
    })
}

/// A buffer element's Go slice spelling. `Bytes` reads as `byte`, the way
/// every other Go API spells a byte slice, rather than `uint8`.
pub(crate) fn element_go(ty: &Ty) -> &'static str {
    match ty {
        Ty::Bytes => "byte",
        Ty::List(inner) => scalar(inner).map_or("byte", |s| s.go),
        _ => "byte",
    }
}

/// The module path's last element, snake_cased into a legal identifier the
/// same way a C identifier is built and escaped against Go's reserved words.
///
/// This is the one derivation for everything Go-facing (the `package`
/// declaration, the file name) and everything the embedded C crate is named
/// after (its Cargo package, its library, the header, the `.pc` file):
/// `opts.module` cannot be trusted here, since `[[bind]]` defaults it from
/// `opts.package` with only hyphens replaced, which does nothing for the
/// dots and slashes a Go module path is full of.
pub(crate) fn package_name(module_path: &str) -> String {
    let last = module_path.rsplit('/').next().unwrap_or(module_path);
    ident(&c::snake(last))
}

/// Words a generated Go declaration will not accept as a name: Go's own
/// keywords, the predeclared universe-block identifiers (a parameter named
/// `error` or `len` would shadow the builtin for the whole function body),
/// plus the receiver, the raw call result and the error pointer every
/// fallible shim already generates around it.
const RESERVED: &[&str] = &[
    "break",
    "default",
    "func",
    "interface",
    "select",
    "case",
    "defer",
    "go",
    "map",
    "struct",
    "chan",
    "else",
    "goto",
    "package",
    "switch",
    "const",
    "fallthrough",
    "if",
    "range",
    "type",
    "continue",
    "for",
    "import",
    "return",
    "var",
    "bool",
    "byte",
    "complex64",
    "complex128",
    "error",
    "float32",
    "float64",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "rune",
    "string",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    "true",
    "false",
    "iota",
    "nil",
    "append",
    "cap",
    "close",
    "complex",
    "copy",
    "delete",
    "imag",
    "len",
    "make",
    "new",
    "panic",
    "print",
    "println",
    "real",
    "recover",
    "recv",
    "ret",
    "errPtr",
];

/// A Rust parameter name as the Go identifier it is declared under.
pub(crate) fn ident(name: &str) -> String {
    crate::naming::escape(name, RESERVED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_go_package_name_is_the_module_paths_last_segment() {
        assert_eq!(package_name("github.com/acme/core"), "core");
        assert_eq!(package_name("acme-core"), "acme_core");
        assert_eq!(
            package_name("github.com/acme/soothfast/soothfast-demo/bindings/gostats"),
            "gostats"
        );
    }

    #[test]
    fn a_package_name_that_collides_with_a_go_word_is_escaped() {
        assert_eq!(package_name("github.com/acme/go"), "go_");
        assert_eq!(package_name("github.com/acme/error"), "error_");
    }

    #[test]
    fn reserved_go_words_gain_a_trailing_underscore() {
        assert_eq!(ident("range"), "range_");
        assert_eq!(ident("recv"), "recv_");
        assert_eq!(ident("by"), "by");
    }
}
