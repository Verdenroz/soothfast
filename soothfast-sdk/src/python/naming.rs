//! Wire names → Python identifiers.

pub(crate) use crate::naming::snake;

/// snake_case plus keyword escaping, for attribute positions.
pub(crate) fn snake_attr(wire: &str) -> String {
    escape_keyword(&snake(wire))
}

/// Append an underscore to Python keywords so the name stays legal.
pub(crate) fn escape_keyword(name: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
        "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
        "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
        "try", "while", "with", "yield",
    ];
    if KEYWORDS.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// A wire name as a legal Python identifier, changed as little as possible.
pub(crate) fn sanitize_ident(wire: &str) -> String {
    escape_keyword(&crate::naming::sanitize(wire))
}

/// A Python string literal. JSON's string escapes are all valid in Python,
/// so `serde_json` renders one directly; non-ASCII stays as UTF-8 text.
pub(crate) fn string_lit(value: &str) -> String {
    serde_json::Value::String(value.to_string()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_gain_a_trailing_underscore() {
        assert_eq!(snake_attr("from"), "from_");
        assert_eq!(snake_attr("class"), "class_");
        assert_eq!(snake_attr("type"), "type");
    }

    #[test]
    fn sanitize_ident_keeps_the_wire_spelling_where_it_is_legal() {
        assert_eq!(sanitize_ident("logoUrl"), "logoUrl");
        assert_eq!(sanitize_ident("Retry-After"), "Retry_After");
        assert_eq!(sanitize_ident("lambda"), "lambda_");
    }

    #[test]
    fn string_lit_escapes_quotes_and_controls_and_keeps_non_ascii() {
        assert_eq!(string_lit("Café"), "\"Café\"");
        assert_eq!(string_lit("say \"hi\""), r#""say \"hi\"""#);
        assert_eq!(string_lit("a\\b\n\u{1}"), r#""a\\b\n\u0001""#);
    }
}
