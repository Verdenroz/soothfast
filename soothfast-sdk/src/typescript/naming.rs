//! Wire names → TypeScript identifiers.
//!
//! Only positions that *must* be identifiers get renamed. Property keys
//! stay wire-faithful (quoted when they have to be), so a parsed JSON
//! response is already the emitted interface — there is no decode step to
//! get wrong.

use crate::naming::{sanitize, snake};

/// camelCase an operation id: `get_batch_quotes` and `GetBatchQuotes` both
/// become `getBatchQuotes`.
///
/// Method position, not binding position — `delete` and `new` are legal
/// class members, so only `constructor` has to give way.
pub(crate) fn camel(name: &str) -> String {
    let flat = snake(name);
    let mut parts = flat.split('_').filter(|p| !p.is_empty());
    let mut out = String::with_capacity(flat.len());
    if let Some(first) = parts.next() {
        out.push_str(first);
    }
    for part in parts {
        let mut chars = part.chars();
        if let Some(c) = chars.next() {
            out.push(c.to_ascii_uppercase());
        }
        out.push_str(chars.as_str());
    }
    if out.is_empty() {
        return "call".into();
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if out == "constructor" {
        out.push('_');
    }
    out
}

/// PascalCase, for type names derived from a method name.
pub(crate) fn pascal(name: &str) -> String {
    let camel = camel(name);
    let mut chars = camel.chars();
    match chars.next() {
        Some(c) => format!("{}{}", c.to_ascii_uppercase(), chars.as_str()),
        None => camel,
    }
}

/// A wire name as a legal TypeScript identifier, changed as little as
/// possible.
pub(crate) fn ident(wire: &str) -> String {
    let out = sanitize(wire);
    if out.is_empty() {
        return "value".into();
    }
    escape_reserved(&out)
}

/// A component name as a type name.
pub(crate) fn type_name(name: &str) -> String {
    ident(name)
}

/// Whether a name can appear unquoted as a property key.
pub(crate) fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// A property key, quoted only when it has to be. Reserved words are legal
/// unquoted in this position, so they are left alone.
pub(crate) fn prop_key(wire: &str) -> String {
    if is_ident(wire) {
        wire.to_string()
    } else {
        string_lit(wire)
    }
}

/// A TypeScript string literal. JSON string escaping is a subset of
/// TypeScript's, so `serde_json` renders one directly.
pub(crate) fn string_lit(value: &str) -> String {
    serde_json::Value::String(value.to_string()).to_string()
}

/// Append an underscore to words TypeScript will not accept as a binding.
fn escape_reserved(name: &str) -> String {
    const RESERVED: &[&str] = &[
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "null",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ];
    if RESERVED.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_ids_reach_the_same_camel_case_from_any_spelling() {
        assert_eq!(camel("getBatchQuotes"), "getBatchQuotes");
        assert_eq!(camel("get_batch_quotes"), "getBatchQuotes");
        assert_eq!(camel("GetBatchQuotes"), "getBatchQuotes");
        assert_eq!(camel("get-batch-quotes"), "getBatchQuotes");
    }

    #[test]
    fn pascal_capitalizes_without_disturbing_the_rest() {
        assert_eq!(pascal("listItems"), "ListItems");
        assert_eq!(pascal("get_item"), "GetItem");
    }

    #[test]
    fn leading_digits_and_reserved_words_never_produce_illegal_bindings() {
        assert_eq!(camel("50DayAverage"), "_50DayAverage");
        assert_eq!(ident("50DayAverage"), "_50DayAverage");
        assert_eq!(ident("new"), "new_");
        assert_eq!(ident("Retry-After"), "Retry_After");
    }

    #[test]
    fn method_names_keep_reserved_words_but_never_shadow_the_constructor() {
        assert_eq!(camel("delete"), "delete");
        assert_eq!(camel("new"), "new");
        assert_eq!(camel("constructor"), "constructor_");
    }

    #[test]
    fn property_keys_stay_wire_faithful_and_quote_only_when_forced() {
        assert_eq!(prop_key("logo_url"), "logo_url");
        assert_eq!(prop_key("logoUrl"), "logoUrl");
        // Reserved words are legal unquoted as keys.
        assert_eq!(prop_key("new"), "new");
        assert_eq!(prop_key("Retry-After"), "\"Retry-After\"");
        assert_eq!(prop_key("50DayAverage"), "\"50DayAverage\"");
    }
}
