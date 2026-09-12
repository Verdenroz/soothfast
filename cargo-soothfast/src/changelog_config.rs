//! The `[changelog]` and `[changelog.icons]` tables of `soothfast.toml`.
//!
//! `soothfast.toml` is shared with the site, spec and gate engines, so this
//! parser skips every table it doesn't own.

use std::collections::BTreeMap;
use std::path::Path;

use soothfast_report::changelog::Icons;
use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

/// Repo-level `report changelog` settings. CLI flags override these.
#[derive(Default, Debug)]
pub struct ChangelogConfig {
    pub icons: Icons,
    /// Cargo features the API surface is read under. Falls back to
    /// `[gate] features` when absent.
    pub features: Option<String>,
    /// Packages `-p` defaults to.
    pub packages: Vec<String>,
    /// Author whose commits are left out of the draft, for a repo that
    /// renamed the bot's `bot-slug`.
    pub bot_author: Option<String>,
}

enum Table {
    Changelog,
    Icons,
    Other,
}

/// Read `[changelog]` and `[changelog.icons]` from a directory's
/// `soothfast.toml`. An absent file just means defaults.
pub fn load(dir: &Path) -> Result<ChangelogConfig, String> {
    let path = dir.join("soothfast.toml");
    match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ChangelogConfig::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
    }
}

/// Parse the `[changelog]` and `[changelog.icons]` tables of a
/// `soothfast.toml`.
pub fn parse(text: &str) -> Result<ChangelogConfig, String> {
    let mut cfg = ChangelogConfig::default();
    let mut overrides = BTreeMap::new();
    let mut table = Table::Other;

    for (lineno, line) in logical_lines(text) {
        let line = line.as_str();
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            table = match inner.trim() {
                "changelog" => Table::Changelog,
                "changelog.icons" => Table::Icons,
                _ => Table::Other,
            };
            continue;
        }
        if let Table::Other = table {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {lineno}: expected `key = value`"));
        };
        let key = key.trim();
        let value = parse_value(value.trim()).map_err(|e| format!("line {lineno}: {e}"))?;
        if let Table::Icons = table {
            let TomlValue::Str(icon) = value else {
                return Err(format!(
                    "line {lineno}: `{key}` under [changelog.icons] must be a string"
                ));
            };
            overrides.insert(key.to_ascii_lowercase(), icon);
        } else {
            set(&mut cfg, key, value).map_err(|e| format!("line {lineno}: {e}"))?;
        }
    }

    cfg.icons = Icons::new(overrides)?;
    Ok(cfg)
}

fn set(cfg: &mut ChangelogConfig, key: &str, value: TomlValue) -> Result<(), String> {
    match (key, value) {
        ("features", TomlValue::Str(s)) => cfg.features = Some(s),
        ("packages", TomlValue::StrArray(v)) => cfg.packages = v,
        ("bot-author", TomlValue::Str(s)) => cfg.bot_author = Some(s),
        (k, _) => return Err(format!("unknown or mistyped `{k}` under [changelog]")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_without_the_table_keeps_the_shipped_icons() {
        let cfg = parse("[site]\nname = \"x\"\n").unwrap();
        assert_eq!(
            format!("{:?}", cfg.icons),
            format!("{:?}", Icons::default())
        );
    }

    #[test]
    fn declared_sections_are_taken_and_the_rest_stay_default() {
        let cfg = parse("[changelog.icons]\nfeatures = \"A\"\n").unwrap();
        assert!(format!("{:?}", cfg.icons).contains("features"));
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

    #[test]
    fn reads_features_and_packages() {
        let cfg =
            parse("[changelog]\nfeatures = \"full\"\npackages = [\"core\", \"server\"]\n").unwrap();
        assert_eq!(cfg.features.as_deref(), Some("full"));
        assert_eq!(cfg.packages, ["core", "server"]);
    }

    #[test]
    fn the_two_changelog_tables_do_not_bleed_into_each_other() {
        let text = "[changelog]\nfeatures = \"full\"\n\n[changelog.icons]\nfeatures = \"A\"\n";
        let cfg = parse(text).unwrap();
        assert_eq!(cfg.features.as_deref(), Some("full"));
        assert!(format!("{:?}", cfg.icons).contains('A'));
    }

    #[test]
    fn reads_a_renamed_bot_author() {
        let cfg = parse("[changelog]\nbot-author = \"acme-bot[bot]\"\n").unwrap();
        assert_eq!(cfg.bot_author.as_deref(), Some("acme-bot[bot]"));
    }

    #[test]
    fn an_unknown_key_under_changelog_is_an_error() {
        let e = parse("[changelog]\nnope = 1\n").unwrap_err();
        assert!(e.contains("unknown or mistyped"), "{e}");
    }

    #[test]
    fn a_mistyped_packages_is_an_error() {
        assert!(parse("[changelog]\npackages = \"core\"\n").is_err());
    }

    #[test]
    fn skips_tables_it_does_not_own() {
        let text = "[gate]\nfeatures = \"bench-gate\"\n\n[changelog]\nfeatures = \"full\"\n";
        assert_eq!(parse(text).unwrap().features.as_deref(), Some("full"));
    }
}
