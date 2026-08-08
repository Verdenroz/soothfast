//! serde helper attributes, recovered from rustdoc JSON `attrs` strings.
//!
//! These describe the wire whenever serde produces it. They do *not* describe
//! a type async-graphql serves — see [`graphql_attrs`](super::graphql_attrs),
//! whose attributes replace these for naming there.

use super::attrs;

/// How a container renames every field or variant beneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rename {
    Lower,
    Upper,
    Pascal,
    Camel,
    Snake,
    ScreamingSnake,
    Kebab,
    ScreamingKebab,
}

impl Rename {
    /// Parse a serde `rename_all` value. Unknown rules are rejected rather
    /// than ignored — a silently dropped rule renames every field wrongly.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "lowercase" => Self::Lower,
            "UPPERCASE" => Self::Upper,
            "PascalCase" => Self::Pascal,
            "camelCase" => Self::Camel,
            "snake_case" => Self::Snake,
            "SCREAMING_SNAKE_CASE" => Self::ScreamingSnake,
            "kebab-case" => Self::Kebab,
            "SCREAMING-KEBAB-CASE" => Self::ScreamingKebab,
            _ => return None,
        })
    }

    /// Apply to a field name, which Rust writes in `snake_case`.
    pub fn apply_to_field(self, name: &str) -> String {
        match self {
            Self::Lower | Self::Snake => name.to_string(),
            Self::Upper => name.to_uppercase(),
            Self::ScreamingSnake => name.to_uppercase(),
            Self::Kebab => name.replace('_', "-"),
            Self::ScreamingKebab => name.to_uppercase().replace('_', "-"),
            Self::Pascal => snake_to_pascal(name),
            Self::Camel => {
                let pascal = snake_to_pascal(name);
                let mut chars = pascal.chars();
                match chars.next() {
                    Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
                    None => pascal,
                }
            }
        }
    }

    /// Apply to a variant name, which Rust writes in `PascalCase`.
    pub fn apply_to_variant(self, name: &str) -> String {
        match self {
            Self::Pascal => name.to_string(),
            Self::Lower => name.to_lowercase(),
            Self::Upper => name.to_uppercase(),
            Self::Camel => {
                let mut chars = name.chars();
                match chars.next() {
                    Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
                    None => name.to_string(),
                }
            }
            Self::Snake => pascal_to_snake(name),
            Self::ScreamingSnake => pascal_to_snake(name).to_uppercase(),
            Self::Kebab => pascal_to_snake(name).replace('_', "-"),
            Self::ScreamingKebab => pascal_to_snake(name).to_uppercase().replace('_', "-"),
        }
    }
}

fn snake_to_pascal(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut capitalize = true;
    for c in name.chars() {
        if c == '_' {
            capitalize = true;
        } else if capitalize {
            out.extend(c.to_uppercase());
            capitalize = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn pascal_to_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (i, c) in name.char_indices() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// Container-level serde options that change the emitted schema.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ContainerAttrs {
    pub rename: Option<String>,
    pub rename_all: Option<Rename>,
    /// `tag = "..."` — internally or adjacently tagged enum representation.
    pub tag: Option<String>,
    /// `content = "..."` — present only for adjacently tagged enums.
    pub content: Option<String>,
    pub untagged: bool,
    pub transparent: bool,
    pub deny_unknown_fields: bool,
    /// An unrecognised `rename_all` value; the caller reports it as a gap
    /// rather than emitting field names it knows are wrong.
    pub unknown_rename_all: Option<String>,
}

/// Field-level serde options that change the emitted schema.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FieldAttrs {
    pub rename: Option<String>,
    pub skip: bool,
    pub flatten: bool,
    /// `default` makes a field optional on the wire even when its type isn't.
    pub default: bool,
    /// `with` / `serialize_with` route the wire shape through arbitrary code,
    /// so the field's Rust type stops predicting its JSON shape.
    pub custom_serializer: Option<String>,
}

/// Pull the `serde(...)` argument text out of rustdoc's attribute entries.
pub fn serde_args(raw: &[serde_json::Value]) -> Vec<String> {
    attrs::args(raw, "serde")
}

/// Parse container-level `#[serde(...)]` attributes.
pub fn container(raw: &[serde_json::Value]) -> ContainerAttrs {
    let mut out = ContainerAttrs::default();
    for args in serde_args(raw) {
        for item in attrs::split_items(&args) {
            match attrs::key_value(item) {
                ("rename", Some(v)) => out.rename = Some(v),
                ("rename_all", Some(v)) => match Rename::parse(&v) {
                    Some(r) => out.rename_all = Some(r),
                    None => out.unknown_rename_all = Some(v),
                },
                ("tag", Some(v)) => out.tag = Some(v),
                ("content", Some(v)) => out.content = Some(v),
                ("untagged", None) => out.untagged = true,
                ("transparent", None) => out.transparent = true,
                ("deny_unknown_fields", None) => out.deny_unknown_fields = true,
                _ => {}
            }
        }
    }
    out
}

/// Parse field-level `#[serde(...)]` attributes.
pub fn field(raw: &[serde_json::Value]) -> FieldAttrs {
    let mut out = FieldAttrs::default();
    for args in serde_args(raw) {
        for item in attrs::split_items(&args) {
            match attrs::key_value(item) {
                ("rename", Some(v)) => out.rename = Some(v),
                ("skip", None) | ("skip_serializing", None) => out.skip = true,
                ("flatten", None) => out.flatten = true,
                ("default", None) | ("default", Some(_)) => out.default = true,
                ("with", Some(v)) | ("serialize_with", Some(v)) => out.custom_serializer = Some(v),
                _ => {}
            }
        }
    }
    out
}

/// The wire name of a field, applying `rename` then the container rule.
pub fn wire_name_field(rust_name: &str, f: &FieldAttrs, c: &ContainerAttrs) -> String {
    match (&f.rename, c.rename_all) {
        (Some(r), _) => r.clone(),
        (None, Some(rule)) => rule.apply_to_field(rust_name),
        (None, None) => rust_name.to_string(),
    }
}

/// The wire name of a variant, applying `rename` then the container rule.
pub fn wire_name_variant(rust_name: &str, v: &ContainerAttrs, c: &ContainerAttrs) -> String {
    match (&v.rename, c.rename_all) {
        (Some(r), _) => r.clone(),
        (None, Some(rule)) => rule.apply_to_variant(rust_name),
        (None, None) => rust_name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn attrs(items: &[&str]) -> Vec<serde_json::Value> {
        items.iter().map(|s| json!({ "other": s })).collect()
    }

    #[test]
    fn reads_rustdoc_object_and_string_attr_shapes() {
        let mixed = vec![
            json!({"other": "#[serde(flatten)]"}),
            json!("#[serde(default)]"),
        ];
        let f = field(&mixed);
        assert!(f.flatten && f.default);
    }

    #[test]
    fn ignores_non_serde_attributes() {
        let f = field(&attrs(&[
            "#[doc = \"hi\"]",
            "#[repr(C)]",
            "#[graphql(name = \"nope\")]",
        ]));
        assert_eq!(f, FieldAttrs::default());
    }

    #[test]
    fn parses_the_field_attrs_that_change_shape() {
        let f = field(&attrs(&["#[serde(rename = \"qty\", default)]"]));
        assert_eq!(f.rename.as_deref(), Some("qty"));
        assert!(f.default);
        assert!(!f.skip);
    }

    #[test]
    fn custom_serializer_is_captured_not_dropped() {
        let f = field(&attrs(&["#[serde(with = \"ts_seconds\")]"]));
        assert_eq!(f.custom_serializer.as_deref(), Some("ts_seconds"));
    }

    #[test]
    fn parses_container_tag_and_rename_all() {
        let c = container(&attrs(&[
            "#[serde(tag = \"kind\", rename_all = \"snake_case\")]",
        ]));
        assert_eq!(c.tag.as_deref(), Some("kind"));
        assert_eq!(c.rename_all, Some(Rename::Snake));
    }

    #[test]
    fn unknown_rename_all_is_reported_not_silently_ignored() {
        let c = container(&attrs(&["#[serde(rename_all = \"Train-Case\")]"]));
        assert_eq!(c.rename_all, None);
        assert_eq!(c.unknown_rename_all.as_deref(), Some("Train-Case"));
    }

    #[test]
    fn field_rename_rules_cover_serde_vocabulary() {
        assert_eq!(Rename::Camel.apply_to_field("item_id"), "itemId");
        assert_eq!(Rename::Pascal.apply_to_field("item_id"), "ItemId");
        assert_eq!(Rename::Kebab.apply_to_field("item_id"), "item-id");
        assert_eq!(Rename::ScreamingSnake.apply_to_field("item_id"), "ITEM_ID");
        assert_eq!(Rename::Snake.apply_to_field("item_id"), "item_id");
    }

    #[test]
    fn variant_rename_rules_start_from_pascal_case() {
        assert_eq!(Rename::Snake.apply_to_variant("NotFound"), "not_found");
        assert_eq!(Rename::Kebab.apply_to_variant("NotFound"), "not-found");
        assert_eq!(Rename::Camel.apply_to_variant("NotFound"), "notFound");
        assert_eq!(Rename::Lower.apply_to_variant("NotFound"), "notfound");
    }

    #[test]
    fn explicit_rename_beats_the_container_rule() {
        let c = ContainerAttrs {
            rename_all: Some(Rename::Camel),
            ..Default::default()
        };
        let f = FieldAttrs {
            rename: Some("qty".into()),
            ..Default::default()
        };
        assert_eq!(wire_name_field("quantity_on_hand", &f, &c), "qty");
        let plain = FieldAttrs::default();
        assert_eq!(
            wire_name_field("quantity_on_hand", &plain, &c),
            "quantityOnHand"
        );
    }
}
