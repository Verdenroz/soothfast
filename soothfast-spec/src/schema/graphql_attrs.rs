//! async-graphql helper attributes, recovered from rustdoc JSON `attrs`.
//!
//! For a type async-graphql serves, the response JSON is produced by
//! async-graphql's resolver rather than by serde, so `#[graphql(...)]` is the
//! wire truth and `#[serde(...)]` never touches it. The two also *disagree*:
//! async-graphql renames through the `Inflector` crate, which opens a new word
//! at every digit, so `price_change_percentage_24h` goes on the wire as
//! `priceChangePercentage24H` where serde would say `...24h`. Routing a
//! GraphQL-served type through serde's casing is therefore wrong, not untidy —
//! and "just add a serde rename" is worse still, because those types are
//! usually `Deserialize` from the *library's* snake_case JSON, where a rename
//! silently zeroes every field instead of erroring.
//!
//! The rules below reproduce `Inflector` exactly, digit quirks and all, so the
//! emitted names match what the server actually sends.

use serde_json::Value;

use super::attrs;

/// How a container renames every field or enum item beneath it.
///
/// These are the six rules async-graphql accepts — notably *not* kebab-case,
/// which serde has and async-graphql does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rename {
    Lower,
    Upper,
    Pascal,
    Camel,
    Snake,
    ScreamingSnake,
}

impl Rename {
    /// Parse a `rename_fields` / `rename_items` value. Unknown rules are
    /// rejected rather than ignored, so the caller can report them.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "lowercase" => Self::Lower,
            "UPPERCASE" => Self::Upper,
            "PascalCase" => Self::Pascal,
            "camelCase" => Self::Camel,
            "snake_case" => Self::Snake,
            "SCREAMING_SNAKE_CASE" => Self::ScreamingSnake,
            _ => return None,
        })
    }

    /// Rename one identifier the way async-graphql does.
    ///
    /// `lowercase`/`UPPERCASE` map the whole string; the other four go through
    /// `Inflector`'s word splitter, which treats every non-alphanumeric run and
    /// every digit as a word boundary.
    pub fn apply(self, name: &str) -> String {
        match self {
            Self::Lower => name.to_lowercase(),
            Self::Upper => name.to_uppercase(),
            Self::Pascal => camel_like(name, true),
            Self::Camel => camel_like(name, false),
            Self::Snake => snake_like(name, false),
            Self::ScreamingSnake => snake_like(name, true),
        }
    }
}

/// `Inflector::to_camel_case` / `to_pascal_case`, which differ only in whether
/// the first word starts a new word (and so gets capitalised).
fn camel_like(name: &str, lead_upper: bool) -> String {
    let body = name.trim_end_matches(|c: char| !c.is_alphanumeric());
    let mut out = String::with_capacity(body.len());
    let (mut new_word, mut last, mut found) = (lead_upper, ' ', false);
    for c in body.chars() {
        if !c.is_alphanumeric() {
            // Leading separators are dropped; later ones open a word.
            if found {
                new_word = true;
            }
        } else if c.is_numeric() {
            // A digit is its own word *and* opens the next one, which is why
            // `_24h` becomes `24H` and not `24h`.
            (found, new_word) = (true, true);
            out.push(c);
        } else if new_word || (last.is_lowercase() && c.is_uppercase()) {
            (found, new_word) = (true, false);
            out.push(c.to_ascii_uppercase());
        } else {
            (found, last) = (true, c);
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// `Inflector::to_snake_case` / `to_screaming_snake_case`.
fn snake_like(name: &str, upper: bool) -> String {
    let chars: Vec<char> = name.chars().collect();
    let end = chars
        .iter()
        .rposition(|c| c.is_alphanumeric())
        .map_or(0, |i| i + 1);
    let cased = |c: char| match upper {
        true => c.to_ascii_uppercase(),
        false => c.to_ascii_lowercase(),
    };
    let mut out = String::with_capacity(name.len() * 2);
    let mut first = true;
    for (i, &c) in chars[..end].iter().enumerate() {
        if !c.is_alphanumeric() {
            if !first {
                first = true;
                out.push('_');
            }
            continue;
        }
        // `Inflector` calls a char "uppercase" when it equals its own ASCII
        // uppercase — which digits do, so `24h` splits as `2_4h`.
        if !first && c == c.to_ascii_uppercase() && neighbour_is_lower(&chars, i) {
            out.push('_');
        }
        first = false;
        out.push(cased(c));
    }
    out
}

fn neighbour_is_lower(chars: &[char], i: usize) -> bool {
    chars.get(i + 1).is_some_and(|c| c.is_lowercase()) || (i > 0 && chars[i - 1].is_lowercase())
}

/// Container-level `#[graphql(...)]` options that change wire names.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ContainerAttrs {
    pub rename_fields: Option<Rename>,
    pub rename_items: Option<Rename>,
    /// An unrecognised rule value; the caller reports it as a gap rather than
    /// emitting names it knows are wrong.
    pub unknown_rename_rule: Option<String>,
    /// Whether any `#[graphql(...)]` attribute was present, which is one of
    /// the two ways a GraphQL-served type gives itself away.
    pub present: bool,
}

impl ContainerAttrs {
    /// The rule applied to struct fields. async-graphql's default is
    /// camelCase, so a type with no attribute at all is still renamed.
    pub fn field_rule(&self) -> Rename {
        self.rename_fields.unwrap_or(Rename::Camel)
    }

    /// The rule applied to enum items, which defaults to SCREAMING_SNAKE_CASE
    /// — a separate rule from the field one, not a fallback to it.
    pub fn item_rule(&self) -> Rename {
        self.rename_items.unwrap_or(Rename::ScreamingSnake)
    }
}

/// Field-level `#[graphql(...)]` options.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FieldAttrs {
    pub name: Option<String>,
    /// `skip` takes the field out of the GraphQL type entirely, so it is not
    /// on the wire under any name.
    pub skip: bool,
}

/// Parse container-level `#[graphql(...)]` attributes.
pub fn container(raw: &[Value]) -> ContainerAttrs {
    let mut out = ContainerAttrs::default();
    for args in attrs::args(raw, "graphql") {
        out.present = true;
        for item in attrs::split_items(&args) {
            let (key, value) = attrs::key_value(item);
            let slot = match key {
                "rename_fields" => &mut out.rename_fields,
                "rename_items" => &mut out.rename_items,
                _ => continue,
            };
            let Some(v) = value else { continue };
            match Rename::parse(&v) {
                Some(r) => *slot = Some(r),
                None => out.unknown_rename_rule = Some(v),
            }
        }
    }
    out
}

/// Parse field-level `#[graphql(...)]` attributes.
pub fn field(raw: &[Value]) -> FieldAttrs {
    let mut out = FieldAttrs::default();
    for args in attrs::args(raw, "graphql") {
        for item in attrs::split_items(&args) {
            match attrs::key_value(item) {
                ("name", Some(v)) => out.name = Some(v),
                ("skip", None) => out.skip = true,
                _ => {}
            }
        }
    }
    out
}

/// Parse variant-level `#[graphql(...)]` attributes. Enum items take `name`
/// but have no `skip`, so the field parser's answer is reused as-is.
pub fn variant(raw: &[Value]) -> FieldAttrs {
    field(raw)
}

/// The wire name of a field: `name` if given, else the container's rule.
pub fn wire_name_field(rust_name: &str, f: &FieldAttrs, c: &ContainerAttrs) -> String {
    match &f.name {
        Some(n) => n.clone(),
        None => c.field_rule().apply(rust_name),
    }
}

/// The wire name of an enum item: `name` if given, else the container's rule.
pub fn wire_name_variant(rust_name: &str, v: &FieldAttrs, c: &ContainerAttrs) -> String {
    match &v.name {
        Some(n) => n.clone(),
        None => c.item_rule().apply(rust_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw(items: &[&str]) -> Vec<Value> {
        items.iter().map(|s| json!({ "other": s })).collect()
    }

    // Every expectation below was read off a live async-graphql 7.2 SDL dump
    // (and `Inflector` called directly), not derived from serde's rules.

    #[test]
    fn camel_case_opens_a_word_at_every_digit() {
        assert_eq!(
            Rename::Camel.apply("price_change_percentage_24h"),
            "priceChangePercentage24H"
        );
        assert_eq!(
            Rename::Camel.apply("market_cap_change_percentage_24h_usd"),
            "marketCapChangePercentage24HUsd"
        );
        assert_eq!(Rename::Camel.apply("x2y"), "x2Y");
        assert_eq!(Rename::Camel.apply("a_1_b"), "a1B");
    }

    #[test]
    fn camel_case_handles_the_degenerate_identifiers() {
        assert_eq!(Rename::Camel.apply("k"), "k");
        assert_eq!(Rename::Camel.apply("report_date"), "reportDate");
        assert_eq!(Rename::Camel.apply("_leading"), "leading");
        assert_eq!(Rename::Camel.apply("double__under"), "doubleUnder");
        assert_eq!(Rename::Camel.apply("trailing_"), "trailing");
        assert_eq!(Rename::Camel.apply("alreadyCamel"), "alreadyCamel");
        assert_eq!(Rename::Camel.apply("FOO_BAR"), "fooBar");
        assert_eq!(Rename::Camel.apply("HTTPServer"), "httpserver");
        assert_eq!(Rename::Camel.apply(""), "");
    }

    #[test]
    fn pascal_case_differs_from_camel_only_in_the_first_word() {
        assert_eq!(Rename::Pascal.apply("report_date"), "ReportDate");
        assert_eq!(
            Rename::Pascal.apply("price_change_percentage_24h"),
            "PriceChangePercentage24H"
        );
        assert_eq!(Rename::Pascal.apply("k"), "K");
        assert_eq!(Rename::Pascal.apply("_leading"), "Leading");
    }

    #[test]
    fn snake_case_splits_digits_the_way_inflector_does() {
        assert_eq!(
            Rename::Snake.apply("price_change_percentage_24h"),
            "price_change_percentage_2_4h"
        );
        assert_eq!(Rename::Snake.apply("alreadyCamel"), "already_camel");
        assert_eq!(Rename::Snake.apply("HTTPServer"), "http_server");
        assert_eq!(Rename::Snake.apply("v2_api"), "v_2_api");
        assert_eq!(Rename::Snake.apply("trailing_"), "trailing");
        assert_eq!(
            Rename::ScreamingSnake.apply("price_change_percentage_24h"),
            "PRICE_CHANGE_PERCENTAGE_2_4H"
        );
        assert_eq!(Rename::ScreamingSnake.apply("NotFound"), "NOT_FOUND");
        assert_eq!(Rename::ScreamingSnake.apply("OkThen"), "OK_THEN");
    }

    #[test]
    fn lowercase_and_uppercase_leave_separators_alone() {
        assert_eq!(Rename::Lower.apply("Report_Date"), "report_date");
        assert_eq!(Rename::Upper.apply("report_date"), "REPORT_DATE");
    }

    #[test]
    fn defaults_are_camel_for_fields_and_screaming_snake_for_items() {
        let c = ContainerAttrs::default();
        assert_eq!(c.field_rule(), Rename::Camel);
        assert_eq!(c.item_rule(), Rename::ScreamingSnake);
    }

    #[test]
    fn container_rules_parse_independently() {
        let c = container(&raw(&[
            "#[graphql(rename_fields = \"snake_case\", rename_items = \"camelCase\")]",
        ]));
        assert_eq!(c.rename_fields, Some(Rename::Snake));
        assert_eq!(c.rename_items, Some(Rename::Camel));
        assert!(c.present);
        assert_eq!(c.unknown_rename_rule, None);
    }

    #[test]
    fn an_unknown_rule_is_reported_and_the_default_kept() {
        // kebab-case is a serde rule async-graphql does not have.
        let c = container(&raw(&["#[graphql(rename_fields = \"kebab-case\")]"]));
        assert_eq!(c.rename_fields, None);
        assert_eq!(c.unknown_rename_rule.as_deref(), Some("kebab-case"));
        assert_eq!(c.field_rule(), Rename::Camel);
    }

    #[test]
    fn serde_attributes_are_not_mistaken_for_graphql_ones() {
        let c = container(&raw(&["#[serde(rename_all = \"snake_case\")]"]));
        assert_eq!(c, ContainerAttrs::default());
        assert!(!c.present);
    }

    #[test]
    fn field_name_beats_the_container_rule() {
        let c = container(&raw(&["#[graphql(rename_fields = \"camelCase\")]"]));
        let f = field(&raw(&["#[graphql(name = \"customName\")]"]));
        assert_eq!(wire_name_field("renamed_field", &f, &c), "customName");
        let plain = FieldAttrs::default();
        assert_eq!(wire_name_field("renamed_field", &plain, &c), "renamedField");
    }

    #[test]
    fn skip_is_read_off_the_field() {
        assert!(field(&raw(&["#[graphql(skip)]"])).skip);
        assert!(!field(&raw(&["#[graphql(name = \"a\")]"])).skip);
    }

    #[test]
    fn variant_name_beats_the_item_rule() {
        let c = container(&raw(&["#[graphql(rename_items = \"camelCase\")]"]));
        assert_eq!(
            wire_name_variant("NotFound", &FieldAttrs::default(), &c),
            "notFound"
        );
        let named = variant(&raw(&["#[graphql(name = \"WEIRD\")]"]));
        assert_eq!(wire_name_variant("Weird", &named, &c), "WEIRD");
    }
}
