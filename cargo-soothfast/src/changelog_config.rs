//! The `[changelog]` section of `soothfast.toml`.
//!
//! `soothfast.toml` is shared with the site, spec and gate engines, so this
//! parser skips every table it doesn't own.

use std::collections::BTreeMap;
use std::path::Path;

use soothfast_report::changelog::Icons;
use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

/// Read `[changelog.icons]` from a directory's `soothfast.toml`. An absent
/// file just means the shipped icons.
pub fn load(dir: &Path) -> Result<Icons, String> {
    let path = dir.join("soothfast.toml");
    match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Icons::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
    }
}

/// Parse the `[changelog.icons]` table of a `soothfast.toml`.
pub fn parse(text: &str) -> Result<Icons, String> {
    let mut overrides = BTreeMap::new();
    let mut in_icons = false;

    for (lineno, line) in logical_lines(text) {
        let line = line.as_str();
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_icons = inner.trim() == "changelog.icons";
            continue;
        }
        if !in_icons {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {lineno}: expected `key = value`"));
        };
        let value = parse_value(value.trim()).map_err(|e| format!("line {lineno}: {e}"))?;
        let TomlValue::Str(icon) = value else {
            return Err(format!(
                "line {lineno}: `{}` under [changelog.icons] must be a string",
                key.trim()
            ));
        };
        overrides.insert(key.trim().to_ascii_lowercase(), icon);
    }
    Icons::new(overrides)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_without_the_table_keeps_the_shipped_icons() {
        let cfg = parse("[site]\nname = \"x\"\n").unwrap();
        assert_eq!(format!("{cfg:?}"), format!("{:?}", Icons::default()));
    }

    #[test]
    fn declared_sections_are_taken_and_the_rest_stay_default() {
        let icons = parse("[changelog.icons]\nfeatures = \"A\"\n").unwrap();
        assert!(format!("{icons:?}").contains("features"));
    }

    #[test]
    fn a_name_that_is_not_a_section_is_an_error_not_a_silent_default() {
        let e = parse("[changelog.icons]\nfeature = \"A\"\n").unwrap_err();
        assert!(e.contains("not a changelog section"), "{e}");
    }

    #[test]
    fn a_non_string_icon_is_rejected() {
        let e = parse("[changelog.icons]\nfixes = 7\n").unwrap_err();
        assert!(e.contains("must be a string"), "{e}");
    }
}
