//! The `[gate]` section of `soothfast.toml`.
//!
//! `soothfast.toml` is shared with the site and spec engines, so this parser
//! skips every table it doesn't own.

use std::path::Path;

use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

use crate::invoke::{self, CommonArgs};

/// Repo-level gate settings. CLI flags override these.
#[derive(Default, Debug, PartialEq)]
pub struct GateConfig {
    pub codegen_units: Option<String>,
}

/// Read `soothfast.toml` from a directory. An absent file just means
/// defaults.
pub fn load(dir: &Path) -> Result<GateConfig, String> {
    let path = dir.join("soothfast.toml");
    match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(GateConfig::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
    }
}

/// Fill in what the CLI did not set from the repo's `soothfast.toml`.
pub fn apply(common: &mut CommonArgs) -> Result<(), String> {
    if common.codegen_units.is_some() {
        return Ok(());
    }
    let Ok(root) = invoke::workspace_root() else {
        return Ok(());
    };
    common.codegen_units = load(&root)?.codegen_units;
    Ok(())
}

/// Parse the `[gate]` section of a `soothfast.toml`.
pub fn parse(text: &str) -> Result<GateConfig, String> {
    let mut cfg = GateConfig::default();
    let mut in_gate = false;

    for (lineno, line) in logical_lines(text) {
        let line = line.as_str();
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_gate = inner.trim() == "gate";
            continue;
        }
        if !in_gate {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {lineno}: expected `key = value`"));
        };
        let value = parse_value(value.trim()).map_err(|e| format!("line {lineno}: {e}"))?;
        set(&mut cfg, key.trim(), value).map_err(|e| format!("line {lineno}: {e}"))?;
    }
    Ok(cfg)
}

fn set(cfg: &mut GateConfig, key: &str, value: TomlValue) -> Result<(), String> {
    match (key, value) {
        ("codegen-units", TomlValue::Int(n)) => cfg.codegen_units = Some(n.to_string()),
        ("codegen-units", TomlValue::Str(s)) => cfg.codegen_units = Some(s),
        (k, _) => return Err(format!("unknown or mistyped `{k}` under [gate]")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_codegen_units() {
        let cfg = parse("[gate]\ncodegen-units = 4\n").unwrap();
        assert_eq!(cfg.codegen_units.as_deref(), Some("4"));
    }

    #[test]
    fn reads_inherit() {
        let cfg = parse("[gate]\ncodegen-units = \"inherit\"\n").unwrap();
        assert_eq!(cfg.codegen_units.as_deref(), Some("inherit"));
    }

    #[test]
    fn skips_tables_it_does_not_own() {
        let text = "[site]\nname = \"x\"\n\n[gate]\ncodegen-units = 2\n\n[[sdk]]\nspec = \"y\"\n";
        assert_eq!(parse(text).unwrap().codegen_units.as_deref(), Some("2"));
    }

    #[test]
    fn absent_section_is_default() {
        assert_eq!(
            parse("[site]\nname = \"x\"\n").unwrap(),
            GateConfig::default()
        );
    }

    #[test]
    fn unknown_key_is_an_error() {
        assert!(parse("[gate]\nnope = 1\n").is_err());
    }
}
