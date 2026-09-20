//! Rust identifiers → target-language identifiers.
//!
//! Exported names are already legal Rust identifiers, so little needs
//! sanitizing: collisions with words a target language reserves (each
//! language's own module says which), and a tuple field's synthetic
//! position name (`"0"`, `"1"`), legal as a Rust tuple index but not as an
//! identifier anywhere else.

/// Escape a name the target language will not accept as-is.
pub(crate) fn escape(name: &str, reserved: &[&str]) -> String {
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        return format!("_{name}");
    }
    if reserved.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// camelCase a snake_case Rust name: `bump_by` → `bumpBy`.
pub(crate) fn camel(name: &str) -> String {
    let mut parts = name.split('_').filter(|p| !p.is_empty());
    let mut out = String::with_capacity(name.len());
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
    out
}

/// A name usable as part of a generated Rust identifier: every character
/// that cannot appear in one is dropped.
pub(crate) fn ident_part(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_joins_snake_case_without_touching_the_first_word() {
        assert_eq!(camel("bump_by"), "bumpBy");
        assert_eq!(camel("value"), "value");
        assert_eq!(camel("read_env_var"), "readEnvVar");
    }

    #[test]
    fn reserved_words_gain_a_trailing_underscore() {
        assert_eq!(escape("from", &["from"]), "from_");
        assert_eq!(escape("bump", &["from"]), "bump");
    }

    #[test]
    fn a_digit_leading_name_gains_a_leading_underscore() {
        assert_eq!(escape("0", &[]), "_0");
        assert_eq!(escape("1", &["from"]), "_1");
    }

    #[test]
    fn ident_parts_drop_everything_illegal() {
        assert_eq!(ident_part("Vec<String>"), "VecString");
        assert_eq!(ident_part("()"), "");
        assert_eq!(ident_part("CounterError"), "CounterError");
    }
}
