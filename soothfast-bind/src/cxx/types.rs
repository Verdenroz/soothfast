//! One type, in the C++ spelling the wrapper declares and the C spelling it
//! calls into.
//!
//! The C side is never re-derived: it comes straight from
//! [`crate::cabi::types`], so a C++ signature can never drift from the
//! header the wrapper includes.

use crate::cabi::types as c;

/// The namespace path's last element, snake_cased into a legal identifier
/// the same way a C identifier is built and escaped against C++'s reserved
/// words.
///
/// This is the one derivation for everything the embedded C crate is named
/// after (its Cargo package, its library, the header, the `.pc` file):
/// `opts.module` cannot be trusted here, since `[[bind]]` defaults it from
/// `opts.package` with only hyphens replaced, which does nothing for the
/// `::` a namespace path is full of.
pub(crate) fn package_name(namespace_path: &str) -> String {
    let last = namespace_path.rsplit("::").next().unwrap_or(namespace_path);
    ident(&c::snake(last))
}

/// Words a generated C++ declaration will not accept as a name: the
/// language's own keywords, plus `error`, the local variable every
/// throwing call already declares to hold the raw C error message.
const RESERVED: &[&str] = &[
    "alignas",
    "alignof",
    "and",
    "and_eq",
    "asm",
    "atomic_cancel",
    "atomic_commit",
    "atomic_noexcept",
    "auto",
    "bitand",
    "bitor",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "char8_t",
    "char16_t",
    "char32_t",
    "class",
    "compl",
    "concept",
    "const",
    "consteval",
    "constexpr",
    "constinit",
    "const_cast",
    "continue",
    "co_await",
    "co_return",
    "co_yield",
    "decltype",
    "default",
    "delete",
    "do",
    "double",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "float",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "not",
    "not_eq",
    "nullptr",
    "operator",
    "or",
    "or_eq",
    "private",
    "protected",
    "public",
    "reflexpr",
    "register",
    "reinterpret_cast",
    "requires",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "static_assert",
    "static_cast",
    "struct",
    "switch",
    "synchronized",
    "template",
    "this",
    "thread_local",
    "throw",
    "true",
    "try",
    "typedef",
    "typeid",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "wchar_t",
    "while",
    "xor",
    "xor_eq",
    "error",
];

/// A Rust name as the C++ identifier it is declared under.
pub(crate) fn ident(name: &str) -> String {
    crate::naming::escape(name, RESERVED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cpp_package_name_is_the_namespace_paths_last_segment() {
        assert_eq!(package_name("acme::core"), "core");
        assert_eq!(package_name("acme-core"), "acme_core");
    }

    #[test]
    fn reserved_cpp_words_gain_a_trailing_underscore() {
        assert_eq!(ident("class"), "class_");
        assert_eq!(ident("error"), "error_");
        assert_eq!(ident("by"), "by");
    }
}
