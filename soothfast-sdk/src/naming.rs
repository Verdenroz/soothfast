//! Wire-name transforms every emitter shares.
//!
//! Language-specific spelling (keyword escaping, casing conventions) lives
//! in each emitter's own `naming` module on top of these.

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

/// A wire name as a bare identifier, changed as little as possible: every
/// character that cannot appear in one becomes `_`, and a leading digit
/// gains a prefix.
pub(crate) fn sanitize(wire: &str) -> String {
    let mut out: String = wire
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// A package name as an environment-variable prefix: `acme-items` →
/// `ACME_ITEMS`, `@acme/items` → `ACME_ITEMS`.
pub(crate) fn env_prefix(package: &str) -> String {
    let out: String = package
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "SOOTHFAST".into()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_prefixes_survive_scopes_and_punctuation() {
        assert_eq!(env_prefix("acme-items"), "ACME_ITEMS");
        assert_eq!(env_prefix("@acme/items"), "ACME_ITEMS");
        assert_eq!(env_prefix("finance_query"), "FINANCE_QUERY");
        assert_eq!(env_prefix("-"), "SOOTHFAST");
    }

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
    fn punctuation_and_leading_digits_never_produce_illegal_idents() {
        assert_eq!(snake("%K"), "k");
        assert_eq!(snake("%D"), "d");
        assert_eq!(snake("50DayAverage"), "_50_day_average");
        assert_eq!(snake("a/b"), "a_b");
        assert_eq!(snake("!!"), "field");
    }

    #[test]
    fn sanitize_changes_only_what_it_must() {
        assert_eq!(sanitize("logo_url"), "logo_url");
        assert_eq!(sanitize("Retry-After"), "Retry_After");
        assert_eq!(sanitize("50DayAverage"), "_50DayAverage");
    }
}
