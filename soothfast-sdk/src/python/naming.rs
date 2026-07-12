//! Wire names → Python identifiers.

/// snake_case an operation id or wire name: `getBatchQuotes` →
/// `get_batch_quotes`, `Retry-After` → `retry_after`.
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
        } else {
            if !out.ends_with('_') {
                out.push('_');
            }
            prev_lower = false;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        return "field".into();
    }
    if trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return format!("_{trimmed}");
    }
    trimmed.to_string()
}

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
    let mut out: String = wire
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    escape_keyword(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_and_pascal_and_kebab_all_flatten() {
        assert_eq!(snake("getBatchQuotes"), "get_batch_quotes");
        assert_eq!(snake("PageInfo"), "page_info");
        assert_eq!(snake("Retry-After"), "retry_after");
        assert_eq!(snake("regularMarketPrice"), "regular_market_price");
        assert_eq!(snake("already_snake"), "already_snake");
    }

    #[test]
    fn consecutive_capitals_stay_together() {
        assert_eq!(snake("SDKName"), "sdkname");
        assert_eq!(snake("logoURL"), "logo_url");
    }

    #[test]
    fn keywords_gain_a_trailing_underscore() {
        assert_eq!(snake_attr("from"), "from_");
        assert_eq!(snake_attr("class"), "class_");
        assert_eq!(snake_attr("type"), "type");
    }

    #[test]
    fn punctuation_and_leading_digits_never_produce_illegal_idents() {
        assert_eq!(snake("%K"), "k");
        assert_eq!(snake("%D"), "d");
        assert_eq!(snake("50DayAverage"), "_50_day_average");
        assert_eq!(snake("a/b"), "a_b");
        assert_eq!(snake("!!"), "field");
    }
}
