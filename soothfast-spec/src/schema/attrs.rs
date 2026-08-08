//! Reading helper attributes back out of rustdoc JSON `attrs` strings.
//!
//! Rustdoc drops the derive itself but preserves its helper attributes
//! verbatim (`#[serde(rename = "qty")]`, `#[graphql(name = "qty")]`), so a
//! wire name is derivable without parsing source. Parsing them back out of a
//! string is the price of that, and the tokenizing half is identical for
//! every helper — only the attribute's name differs.

use serde_json::Value;

/// Pull one named attribute's argument text out of rustdoc's entries.
///
/// Rustdoc emits each attribute either as a bare string or as an object with
/// an `other` key; both shapes appear across versions, so both are accepted.
pub(super) fn args(attrs: &[Value], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for attr in attrs {
        let text = match attr {
            Value::String(s) => s.as_str(),
            Value::Object(o) => match o.get("other").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => continue,
            },
            _ => continue,
        };
        if let Some(inner) = strip_wrapper(text, name) {
            out.push(inner.to_string());
        }
    }
    out
}

/// `#[name(a, b)]` -> `a, b`. Returns `None` for any other attribute.
fn strip_wrapper<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let t = text.trim();
    let t = t.strip_prefix("#[")?.strip_suffix(']')?.trim();
    let t = t.strip_prefix(name)?.trim();
    t.strip_prefix('(')?.strip_suffix(')')
}

/// Split on top-level commas, respecting nesting and string literals so
/// `rename(serialize = "a,b")` stays one item.
pub(super) fn split_items(args: &str) -> Vec<&str> {
    let (mut out, mut depth, mut in_str, mut escaped, mut start) =
        (Vec::new(), 0i32, false, false, 0);
    for (i, c) in args.char_indices() {
        if in_str {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(args[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = args[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

/// Split `key = "value"` into its parts, unquoting the value.
pub(super) fn key_value(item: &str) -> (&str, Option<String>) {
    match item.split_once('=') {
        None => (item.trim(), None),
        Some((k, v)) => {
            let v = v.trim();
            let unquoted = v.strip_prefix('"').and_then(|s| s.strip_suffix('"'));
            (k.trim(), Some(unquoted.unwrap_or(v).to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_rustdoc_object_and_string_attr_shapes() {
        let mixed = vec![
            json!({"other": "#[serde(flatten)]"}),
            json!("#[serde(default)]"),
        ];
        assert_eq!(args(&mixed, "serde"), vec!["flatten", "default"]);
    }

    #[test]
    fn only_the_named_attribute_is_returned() {
        let mixed = vec![
            json!({"other": "#[serde(default)]"}),
            json!({"other": "#[graphql(skip)]"}),
            json!({"other": "#[doc = \"hi\"]"}),
        ];
        assert_eq!(args(&mixed, "graphql"), vec!["skip"]);
    }

    #[test]
    fn nested_parens_do_not_split_early() {
        let items = split_items("rename(serialize = \"a\", deserialize = \"b\"), default");
        assert_eq!(items.len(), 2);
        assert_eq!(items[1], "default");
    }

    #[test]
    fn commas_inside_string_literals_do_not_split() {
        let items = split_items("rename = \"a,b\", default");
        assert_eq!(items, vec!["rename = \"a,b\"", "default"]);
    }

    #[test]
    fn key_value_unquotes_and_trims() {
        assert_eq!(
            key_value("rename = \"qty\""),
            ("rename", Some("qty".into()))
        );
        assert_eq!(key_value("default"), ("default", None));
    }
}
