//! `[[spec]]` entries in `soothfast.toml`.
//!
//! `soothfast.toml` is shared with the site engine, so this parser skips
//! every table it doesn't own rather than rejecting unknown ones — the two
//! consumers each read their own sections and ignore the rest.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use soothfast_site::toml::{TomlValue, logical_lines, parse_value};
use soothfast_spec::SpecKind;

/// What soothfast does with a spec file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Generate the file from the code (the code is the source of truth).
    Generate,
    /// Reconcile the code against it (the file is the source of truth).
    Check,
}

/// One declared spec file and the metadata that can't be derived from code.
#[derive(Debug, Clone)]
pub struct SpecEntry {
    /// Path relative to the package directory.
    pub path: String,
    pub mode: Mode,
    /// Which dialect to emit. `None` means "whatever the path implies", so a
    /// file named for its dialect needs no key at all.
    pub dialect: Option<SpecKind>,
    pub title: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub servers: Vec<String>,
    /// `[spec.types]`: canonical type path → JSON Schema, extending the
    /// built-in table for foreign types soothfast cannot walk.
    pub types: BTreeMap<String, Value>,
    /// `[spec.responses]`: status code → response merged into every
    /// operation that doesn't already declare that status.
    pub responses: BTreeMap<String, CommonResponse>,
}

/// One `[spec.responses]` entry: a response every operation shares.
#[derive(Debug, Clone, Default)]
pub struct CommonResponse {
    /// Body type name, resolved against the crate like a `response =`
    /// override; `None` means the response has no body.
    pub type_name: Option<String>,
    pub description: Option<String>,
    /// Response header names.
    pub headers: Vec<String>,
}

impl SpecEntry {
    /// The dialect this file is written in, explicit or inferred.
    pub fn dialect(&self) -> SpecKind {
        self.dialect
            .unwrap_or_else(|| SpecKind::for_path(&self.path))
    }

    fn new(path: String) -> Self {
        // Check is the default because generation overwrites the file: a
        // mistyped mode must never silently clobber a hand-authored spec.
        SpecEntry {
            path,
            mode: Mode::Check,
            dialect: None,
            title: None,
            version: None,
            description: None,
            servers: Vec::new(),
            types: BTreeMap::new(),
            responses: BTreeMap::new(),
        }
    }
}

/// Every `[[spec]]` entry, in declaration order.
#[derive(Debug, Clone, Default)]
pub struct SpecConfig {
    pub entries: Vec<SpecEntry>,
}

impl SpecConfig {
    /// The entry governing a spec file, if one is declared.
    pub fn for_path(&self, path: &str) -> Option<&SpecEntry> {
        self.entries.iter().find(|e| e.path == path)
    }

    /// Whether a spec file should be generated. Files with no entry are
    /// checked, preserving the behaviour of every spec that predates this.
    pub fn mode_of(&self, path: &str) -> Mode {
        self.for_path(path).map(|e| e.mode).unwrap_or(Mode::Check)
    }

    /// The dialect a spec file is written in.
    pub fn dialect_of(&self, path: &str) -> SpecKind {
        self.for_path(path)
            .map(SpecEntry::dialect)
            .unwrap_or_else(|| SpecKind::for_path(path))
    }
}

/// Read `soothfast.toml` from a directory. An absent file is not an error —
/// it just means nothing is configured for generation.
pub fn load(dir: &Path) -> Result<SpecConfig, String> {
    let path = dir.join("soothfast.toml");
    match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SpecConfig::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
    }
}

/// Parse the `[[spec]]` sections of a `soothfast.toml`.
pub fn parse(text: &str) -> Result<SpecConfig, String> {
    let mut cfg = SpecConfig::default();
    let mut section = Section::Other;

    for (lineno, line) in logical_lines(text) {
        let line = line.as_str();

        if let Some(inner) = line.strip_prefix("[[").and_then(|l| l.strip_suffix("]]")) {
            section = if inner.trim() == "spec" {
                cfg.entries.push(SpecEntry::new(String::new()));
                Section::Spec
            } else {
                Section::Other
            };
            continue;
        }
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            // `[spec.types]` is a sub-table of the preceding [[spec]] entry.
            section = match inner.trim() {
                "spec.types" if !cfg.entries.is_empty() => Section::Types,
                "spec.responses" if !cfg.entries.is_empty() => Section::Responses,
                _ => Section::Other,
            };
            continue;
        }
        if section == Section::Other {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {lineno}: expected `key = value`"));
        };
        let value = parse_value(value.trim()).map_err(|e| format!("line {lineno}: {e}"))?;
        let entry = cfg
            .entries
            .last_mut()
            .ok_or_else(|| format!("line {lineno}: key outside [[spec]]"))?;
        match section {
            Section::Spec => {
                set(entry, key.trim(), value).map_err(|e| format!("line {lineno}: {e}"))?
            }
            Section::Types => {
                let path = key.trim().trim_matches('"').to_string();
                let TomlValue::Table(_) = &value else {
                    return Err(format!(
                        "line {lineno}: [spec.types] entry {path:?} must be an inline table, \
                         e.g. {{ type = \"string\", format = \"uuid\" }}"
                    ));
                };
                entry.types.insert(path, to_json(value));
            }
            Section::Responses => {
                let code = key.trim().trim_matches('"').to_string();
                let TomlValue::Table(t) = value else {
                    return Err(format!(
                        "line {lineno}: [spec.responses] entry {code:?} must be an inline \
                         table, e.g. {{ type = \"Error\", headers = [\"Retry-After\"] }}"
                    ));
                };
                let mut cr = CommonResponse::default();
                for (k, v) in t {
                    match (k.as_str(), v) {
                        ("type", TomlValue::Str(s)) => cr.type_name = Some(s),
                        ("description", TomlValue::Str(s)) => cr.description = Some(s),
                        ("headers", TomlValue::StrArray(a)) => cr.headers = a,
                        (k, _) => {
                            return Err(format!(
                                "line {lineno}: unknown or mistyped [spec.responses] key {k:?}"
                            ));
                        }
                    }
                }
                entry.responses.insert(code, cr);
            }
            Section::Other => {}
        }
    }

    for (i, e) in cfg.entries.iter().enumerate() {
        if e.path.is_empty() {
            return Err(format!("[[spec]] entry {} has no `path`", i + 1));
        }
    }
    Ok(cfg)
}

/// Which part of the file the parser is currently reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Spec,
    Types,
    Responses,
    /// A table belonging to another consumer of `soothfast.toml`.
    Other,
}

/// A parsed TOML value as JSON, for schemas that go straight into the spec.
fn to_json(value: TomlValue) -> Value {
    match value {
        TomlValue::Str(s) => Value::String(s),
        TomlValue::Bool(b) => Value::Bool(b),
        TomlValue::Int(n) => Value::Number(n.into()),
        TomlValue::Float(f) => f
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Null, Value::Number),
        TomlValue::StrArray(a) => Value::Array(a.into_iter().map(Value::String).collect()),
        TomlValue::Table(t) => Value::Object(t.into_iter().map(|(k, v)| (k, to_json(v))).collect()),
    }
}

fn set(entry: &mut SpecEntry, key: &str, value: TomlValue) -> Result<(), String> {
    match (key, value) {
        ("path", TomlValue::Str(s)) => entry.path = s,
        ("mode", TomlValue::Str(s)) => {
            entry.mode = match s.as_str() {
                "generate" => Mode::Generate,
                "check" => Mode::Check,
                other => {
                    return Err(format!(
                        "unknown mode {other:?} (expected \"generate\" or \"check\")"
                    ));
                }
            }
        }
        ("dialect", TomlValue::Str(s)) => {
            entry.dialect = Some(SpecKind::parse_name(&s).ok_or_else(|| {
                let known: Vec<&str> = SpecKind::ALL.iter().map(|k| k.name()).collect();
                format!(
                    "unknown dialect {s:?} (expected one of {})",
                    known.join(", ")
                )
            })?)
        }
        ("title", TomlValue::Str(s)) => entry.title = Some(s),
        ("version", TomlValue::Str(s)) => entry.version = Some(s),
        ("description", TomlValue::Str(s)) => entry.description = Some(s),
        ("servers", TomlValue::StrArray(a)) => entry.servers = a,
        (k, _) => return Err(format!("unknown or mistyped [[spec]] key {k:?}")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_generate_entry_with_metadata() {
        let cfg = parse(
            r#"
[[spec]]
path = "specs/openapi.yaml"
mode = "generate"
title = "Items API"
version = "2.1"
servers = ["https://api.example.com", "https://staging.example.com"]
"#,
        )
        .expect("parses");
        assert_eq!(cfg.entries.len(), 1);
        let e = &cfg.entries[0];
        assert_eq!(e.path, "specs/openapi.yaml");
        assert_eq!(e.mode, Mode::Generate);
        assert_eq!(e.title.as_deref(), Some("Items API"));
        assert_eq!(e.servers.len(), 2);
    }

    #[test]
    fn an_explicit_dialect_key_wins_over_the_filename() {
        let cfg = parse(
            "[[spec]]\npath = \"specs/api.yaml\"\nmode = \"generate\"\ndialect = \"asyncapi\"\n",
        )
        .expect("parses");
        assert_eq!(cfg.dialect_of("specs/api.yaml"), SpecKind::AsyncApi);
    }

    #[test]
    fn the_filename_names_the_dialect_when_no_key_does() {
        let cfg =
            parse("[[spec]]\npath = \"schema.graphql\"\nmode = \"generate\"\n").expect("parses");
        assert_eq!(cfg.dialect_of("schema.graphql"), SpecKind::GraphQl);
        // A file with no entry at all is still classified, for messages.
        assert_eq!(
            SpecConfig::default().dialect_of("specs/mcp.json"),
            SpecKind::McpTools
        );
    }

    #[test]
    fn an_unknown_dialect_lists_the_ones_that_exist() {
        let err = parse("[[spec]]\npath = \"a.yaml\"\ndialect = \"grpc\"\n").expect_err("rejected");
        assert!(err.contains("grpc"), "{err}");
        assert!(err.contains("openapi"), "names the alternatives: {err}");
    }

    #[test]
    fn a_missing_mode_defaults_to_check_so_nothing_is_clobbered() {
        let cfg = parse("[[spec]]\npath = \"vendor/stripe.yaml\"\n").expect("parses");
        assert_eq!(cfg.entries[0].mode, Mode::Check);
        assert_eq!(cfg.mode_of("vendor/stripe.yaml"), Mode::Check);
    }

    #[test]
    fn a_spec_file_with_no_entry_is_checked() {
        let cfg = SpecConfig::default();
        assert_eq!(cfg.mode_of("specs/anything.yaml"), Mode::Check);
    }

    #[test]
    fn sections_belonging_to_the_site_engine_are_ignored() {
        let cfg = parse(
            r##"
[site]
title = "Docs"
nav_depth = "3"

[[site.nav]]
title = "Guide"

[[spec]]
path = "specs/openapi.yaml"
mode = "generate"

[site.theme]
primary = "#4f46e5"
"##,
        )
        .expect("site tables are skipped, not rejected");
        assert_eq!(cfg.entries.len(), 1);
        assert_eq!(cfg.entries[0].mode, Mode::Generate);
    }

    #[test]
    fn several_entries_keep_their_own_settings() {
        let cfg = parse(
            r#"
[[spec]]
path = "specs/openapi.yaml"
mode = "generate"

[[spec]]
path = "vendor/stripe.yaml"
mode = "check"
"#,
        )
        .expect("parses");
        assert_eq!(cfg.mode_of("specs/openapi.yaml"), Mode::Generate);
        assert_eq!(cfg.mode_of("vendor/stripe.yaml"), Mode::Check);
    }

    #[test]
    fn an_entry_without_a_path_is_rejected() {
        let err = parse("[[spec]]\nmode = \"generate\"\n").expect_err("path is required");
        assert!(err.contains("no `path`"), "got {err}");
    }

    #[test]
    fn an_unknown_mode_is_rejected_rather_than_assumed() {
        let err = parse("[[spec]]\npath = \"a.yaml\"\nmode = \"regenerate\"\n")
            .expect_err("typo must not pass");
        assert!(err.contains("regenerate"), "got {err}");
    }

    #[test]
    fn an_unknown_key_is_rejected() {
        let err = parse("[[spec]]\npath = \"a.yaml\"\nfomat = \"yaml\"\n")
            .expect_err("typo must not pass silently");
        assert!(err.contains("fomat"), "got {err}");
    }
}

#[cfg(test)]
mod types_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_inline_table_type_overrides() {
        let cfg = parse(
            r#"
[[spec]]
path = "specs/openapi.yaml"
mode = "generate"

[spec.types]
"uuid::Uuid" = { type = "string", format = "uuid" }
"rust_decimal::Decimal" = { type = "string" }
"#,
        )
        .expect("parses");
        let e = &cfg.entries[0];
        assert_eq!(
            e.types["uuid::Uuid"],
            json!({ "type": "string", "format": "uuid" })
        );
        assert_eq!(
            e.types["rust_decimal::Decimal"],
            json!({ "type": "string" })
        );
    }

    #[test]
    fn type_overrides_attach_to_the_preceding_spec_entry() {
        let cfg = parse(
            r#"
[[spec]]
path = "a.yaml"
mode = "generate"

[spec.types]
"a::A" = { type = "string" }

[[spec]]
path = "b.yaml"
mode = "generate"
"#,
        )
        .expect("parses");
        assert_eq!(cfg.for_path("a.yaml").expect("a").types.len(), 1);
        assert_eq!(cfg.for_path("b.yaml").expect("b").types.len(), 0);
    }

    #[test]
    fn reads_common_response_entries() {
        let cfg = parse(
            r#"
[[spec]]
path = "openapi.yaml"
mode = "generate"

[spec.responses]
"429" = { type = "Error", description = "Rate limited", headers = ["Retry-After"] }
"500" = { type = "Error" }
"#,
        )
        .expect("parses");
        let e = &cfg.entries[0];
        let r429 = &e.responses["429"];
        assert_eq!(r429.type_name.as_deref(), Some("Error"));
        assert_eq!(r429.description.as_deref(), Some("Rate limited"));
        assert_eq!(r429.headers, vec!["Retry-After".to_string()]);
        let r500 = &e.responses["500"];
        assert_eq!(r500.type_name.as_deref(), Some("Error"));
        assert!(r500.description.is_none());
        assert!(r500.headers.is_empty());
    }

    #[test]
    fn a_scalar_common_response_is_rejected() {
        let err = parse("[[spec]]\npath = \"a.yaml\"\n\n[spec.responses]\n\"429\" = \"Error\"\n")
            .expect_err("a bare string is not a response");
        assert!(err.contains("inline table"), "got {err}");
    }

    #[test]
    fn an_unknown_common_response_key_is_rejected() {
        let err = parse(
            "[[spec]]\npath = \"a.yaml\"\n\n[spec.responses]\n\"429\" = { tpye = \"Error\" }\n",
        )
        .expect_err("typo must not pass silently");
        assert!(err.contains("tpye"), "got {err}");
    }

    #[test]
    fn a_scalar_type_override_is_rejected() {
        let err = parse("[[spec]]\npath = \"a.yaml\"\n\n[spec.types]\n\"a::A\" = \"string\"\n")
            .expect_err("a bare string is not a schema");
        assert!(err.contains("inline table"), "got {err}");
    }

    #[test]
    fn nested_inline_tables_round_trip() {
        let cfg = parse(
            "[[spec]]\npath = \"a.yaml\"\n\n[spec.types]\n\
             \"a::A\" = { type = \"array\", items = { type = \"string\" } }\n",
        )
        .expect("parses");
        assert_eq!(
            cfg.entries[0].types["a::A"],
            json!({ "type": "array", "items": { "type": "string" } })
        );
    }
}
