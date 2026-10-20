//! The `[test]` section of `soothfast.toml`.
//!
//! `soothfast.toml` is shared with the site and spec engines, so this parser
//! skips every table it doesn't own.

use std::path::Path;

use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

/// Repo-level settings for `cargo soothfast test`.
#[derive(Default, Debug, PartialEq)]
pub struct TestConfig {
    /// Changed files that never select anything, even outside a package.
    pub ignore: Vec<String>,
    /// Files outside a package dir that a pass cache key also hashes.
    pub inputs: Vec<String>,
    /// Environment variables whose values a pass cache key includes.
    pub env: Vec<String>,
}

/// Read `soothfast.toml` from a directory. An absent file just means
/// defaults.
pub fn load(dir: &Path) -> Result<TestConfig, String> {
    let path = dir.join("soothfast.toml");
    match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TestConfig::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
    }
}

/// Parse the `[test]` section of a `soothfast.toml`.
pub fn parse(text: &str) -> Result<TestConfig, String> {
    let mut cfg = TestConfig::default();
    let mut in_test = false;

    for (lineno, line) in logical_lines(text) {
        let line = line.as_str();
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_test = inner.trim() == "test";
            continue;
        }
        if !in_test {
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

fn set(cfg: &mut TestConfig, key: &str, value: TomlValue) -> Result<(), String> {
    match (key, value) {
        ("ignore", TomlValue::StrArray(v)) => cfg.ignore = v,
        ("inputs", TomlValue::StrArray(v)) => cfg.inputs = v,
        ("env", TomlValue::StrArray(v)) => cfg.env = v,
        (k, _) => return Err(format!("unknown or mistyped `{k}` under [test]")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_list() {
        let text = "[test]\nignore = [\"docs/**\", \"*.md\"]\ninputs = [\"assets/**\"]\nenv = [\"HOME\"]\n";
        let cfg = parse(text).unwrap();
        assert_eq!(cfg.ignore, ["docs/**", "*.md"]);
        assert_eq!(cfg.inputs, ["assets/**"]);
        assert_eq!(cfg.env, ["HOME"]);
    }

    #[test]
    fn skips_tables_it_does_not_own() {
        let text = "[gate]\nfeatures = \"x\"\n\n[test]\nenv = [\"A\"]\n\n[[sdk]]\nspec = \"y\"\n";
        assert_eq!(parse(text).unwrap().env, ["A"]);
    }

    #[test]
    fn absent_section_is_default() {
        assert_eq!(
            parse("[site]\nname = \"x\"\n").unwrap(),
            TestConfig::default()
        );
    }

    #[test]
    fn a_string_where_a_list_belongs_is_an_error() {
        assert!(parse("[test]\nignore = \"docs\"\n").is_err());
    }

    #[test]
    fn unknown_key_is_an_error() {
        assert!(parse("[test]\nnope = []\n").is_err());
    }
}
