//! `soothfast.toml` — site configuration. Hand-rolled TOML subset (tables,
//! array-of-tables, strings, booleans, string arrays): enough for site
//! config without a toml dependency, strict errors on anything else.

use crate::toml::{TomlValue, logical_lines, parse_value};
use std::path::PathBuf;

/// One nav group in the sidebar: a titled list of markdown pages.
#[derive(Debug, Clone)]
pub struct NavGroup {
    /// Group heading shown above the pages ("Guide", "API reference", ...).
    pub title: String,
    /// Page paths relative to the docs directory ("index.md", "perf/summary.md").
    pub pages: Vec<String>,
}

/// The `[site.theme]` section: seed hex colors for the brand roles. Any
/// unset field keeps the built-in "gauge indigo" default from `tokens.css`.
/// When at least one is set, the build generates `_soothfast/theme-vars.css`
/// with the full light+dark Material role set derived from each seed
/// (see `crate::color::role_from_seed`).
#[derive(Debug, Clone, Default)]
pub struct ThemeConfig {
    /// Seed hex for `--primary` (brand/interactive color).
    pub primary: Option<String>,
    /// Seed hex for `--secondary` (lower-emphasis interactive states).
    pub secondary: Option<String>,
    /// Seed hex for `--tertiary` (reserved spot color).
    pub tertiary: Option<String>,
    /// Seed hex for `--background`/`--surface` (page ground tones).
    pub background: Option<String>,
}

impl ThemeConfig {
    /// True when every role keeps the built-in default.
    pub fn is_default(&self) -> bool {
        self.primary.is_none()
            && self.secondary.is_none()
            && self.tertiary.is_none()
            && self.background.is_none()
    }
}

/// The `[site]` section of `soothfast.toml`, with defaults for every field.
#[derive(Debug, Clone)]
pub struct SiteConfig {
    /// Site name shown in the header wordmark.
    pub name: String,
    /// Version string shown next to the wordmark (usually the crate version).
    pub version: String,
    /// Repository URL for the header link; empty hides the link.
    pub repo: String,
    /// Header brand image, relative to `docs_dir` (e.g. "assets/logo.png").
    /// Unset falls back to the built-in soothfast mark.
    pub logo: Option<String>,
    /// Favicon asset, relative to `docs_dir`. Unset renders no favicon link.
    pub favicon: Option<String>,
    /// Source directory scanned for pages and assets.
    pub docs_dir: PathBuf,
    /// Output directory for the built site.
    pub out_dir: PathBuf,
    /// Optional directory overlaying theme files (templates, assets, icons).
    pub theme_dir: Option<PathBuf>,
    /// Extra stylesheets copied into the site and linked after the theme's.
    pub extra_css: Vec<String>,
    /// Extra scripts copied into the site and loaded after the theme's.
    pub extra_js: Vec<String>,
    /// Whether the built-in search plugin runs.
    pub search: bool,
    /// Explicit navigation; empty means auto-discover from the docs tree.
    pub nav: Vec<NavGroup>,
    /// Brand color role overrides (`[site.theme]`).
    pub theme: ThemeConfig,
}

impl Default for SiteConfig {
    fn default() -> Self {
        SiteConfig {
            name: "docs".into(),
            version: String::new(),
            repo: String::new(),
            logo: None,
            favicon: None,
            docs_dir: PathBuf::from("docs"),
            out_dir: PathBuf::from("site"),
            theme_dir: None,
            extra_css: Vec::new(),
            extra_js: Vec::new(),
            search: true,
            nav: Vec::new(),
            theme: ThemeConfig::default(),
        }
    }
}

/// Tables belonging to the other consumers of the shared `soothfast.toml`.
/// Rejecting these would make a spec- or SDK-configured repo unable to build
/// its site at all.
const FOREIGN_TABLES: [&str; 3] = ["spec", "sdk", "gate"];

fn is_foreign(table: &[String]) -> bool {
    table
        .first()
        .is_some_and(|t| FOREIGN_TABLES.contains(&t.as_str()))
}

/// Parse `soothfast.toml` text into a config. Unknown keys under `[site]` are
/// errors: silently ignored config is how sites drift from intent.
pub fn parse(text: &str) -> Result<SiteConfig, String> {
    let mut cfg = SiteConfig::default();
    // Current table path, e.g. ["site"] or ["site", "nav"] for [[site.nav]].
    let mut table: Vec<String> = Vec::new();
    let mut foreign = false;

    for (lineno, line) in logical_lines(text) {
        let line = line.as_str();

        if let Some(inner) = line.strip_prefix("[[").and_then(|l| l.strip_suffix("]]")) {
            table = inner.split('.').map(|s| s.trim().to_string()).collect();
            foreign = is_foreign(&table);
            if table == ["site", "nav"] {
                cfg.nav.push(NavGroup {
                    title: String::new(),
                    pages: Vec::new(),
                });
            } else if !foreign {
                return Err(format!("line {lineno}: unknown array table [[{inner}]]"));
            }
            continue;
        }
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            table = inner.split('.').map(|s| s.trim().to_string()).collect();
            foreign = is_foreign(&table);
            if !foreign && table != ["site"] && table != ["site", "theme"] {
                return Err(format!("line {lineno}: unknown table [{inner}]"));
            }
            continue;
        }
        if foreign {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {lineno}: expected `key = value`"));
        };
        let key = key.trim();
        let value = parse_value(value.trim()).map_err(|e| format!("line {lineno}: {e}"))?;
        set(&mut cfg, &table, key, value).map_err(|e| format!("line {lineno}: {e}"))?;
    }
    for (n, g) in cfg.nav.iter().enumerate() {
        if g.title.is_empty() {
            return Err(format!("[[site.nav]] entry {} has no title", n + 1));
        }
    }
    Ok(cfg)
}

fn set(cfg: &mut SiteConfig, table: &[String], key: &str, value: TomlValue) -> Result<(), String> {
    if table == ["site", "nav"] {
        let group = cfg.nav.last_mut().ok_or("nav key outside [[site.nav]]")?;
        return match (key, value) {
            ("title", TomlValue::Str(s)) => {
                group.title = s;
                Ok(())
            }
            ("pages", TomlValue::StrArray(a)) => {
                group.pages = a;
                Ok(())
            }
            _ => Err(format!("unknown or mistyped [[site.nav]] key {key:?}")),
        };
    }
    if table == ["site", "theme"] {
        let TomlValue::Str(hex) = value else {
            return Err(format!("[site.theme] key {key:?} must be a hex string"));
        };
        crate::color::parse_hex(&hex).map_err(|e| format!("[site.theme] {key}: {e}"))?;
        match key {
            "primary" => cfg.theme.primary = Some(hex),
            "secondary" => cfg.theme.secondary = Some(hex),
            "tertiary" => cfg.theme.tertiary = Some(hex),
            "background" => cfg.theme.background = Some(hex),
            _ => return Err(format!("unknown [site.theme] key {key:?}")),
        }
        return Ok(());
    }
    if table != ["site"] {
        return Err(format!("key {key:?} outside [site]"));
    }
    match (key, value) {
        ("name", TomlValue::Str(s)) => cfg.name = s,
        ("version", TomlValue::Str(s)) => cfg.version = s,
        ("repo", TomlValue::Str(s)) => cfg.repo = s,
        ("logo", TomlValue::Str(s)) => cfg.logo = Some(s),
        ("favicon", TomlValue::Str(s)) => cfg.favicon = Some(s),
        ("docs_dir", TomlValue::Str(s)) => cfg.docs_dir = PathBuf::from(s),
        ("out_dir", TomlValue::Str(s)) => cfg.out_dir = PathBuf::from(s),
        ("theme_dir", TomlValue::Str(s)) => cfg.theme_dir = Some(PathBuf::from(s)),
        ("extra_css", TomlValue::StrArray(a)) => cfg.extra_css = a,
        ("extra_js", TomlValue::StrArray(a)) => cfg.extra_js = a,
        ("search", TomlValue::Bool(b)) => cfg.search = b,
        _ => return Err(format!("unknown or mistyped [site] key {key:?}")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn tables_owned_by_other_engines_are_skipped() {
        let text = "[site]\nname = \"x\"\n\n[gate]\ncodegen-units = 1\n\n[[spec]]\npath = \"a\"\n\n[[sdk]]\nspec = \"a\"\n";
        let cfg = parse(text).expect("foreign tables must not break the site build");
        assert_eq!(cfg.name, "x");
    }

    #[test]
    fn genuinely_unknown_tables_are_still_errors() {
        assert!(parse("[site]\nname = \"x\"\n\n[nonsense]\nk = 1\n").is_err());
    }
    use super::*;

    #[test]
    fn parses_full_config() {
        let cfg = parse(
            r#"
# site config
[site]
name = "soothfast"          # wordmark
version = "0.1.0"
repo = "https://github.com/Verdenroz/soothfast"
search = false
extra_css = ["docs/a.css", "docs/b.css"]

[[site.nav]]
title = "Guide"
pages = ["index.md", "measuring.md"]

[[site.nav]]
title = "Performance"
pages = ["perf/summary.md"]
"#,
        )
        .unwrap();
        assert_eq!(cfg.name, "soothfast");
        assert!(!cfg.search);
        assert_eq!(cfg.extra_css, vec!["docs/a.css", "docs/b.css"]);
        assert_eq!(cfg.nav.len(), 2);
        assert_eq!(cfg.nav[0].title, "Guide");
        assert_eq!(cfg.nav[1].pages, vec!["perf/summary.md"]);
    }

    #[test]
    fn defaults_apply_without_config() {
        let cfg = parse("").unwrap();
        assert_eq!(cfg.name, "docs");
        assert!(cfg.search);
        assert!(cfg.nav.is_empty());
        assert_eq!(cfg.out_dir, PathBuf::from("site"));
    }

    #[test]
    fn rejects_unknown_keys_and_tables() {
        assert!(parse("[site]\nnmae = \"typo\"").is_err());
        assert!(parse("[other]\nx = \"1\"").is_err());
        assert!(parse("[site]\nname = 42").is_err());
        assert!(parse("[[site.nav]]\npages = [\"a.md\"]").is_err()); // no title
    }

    #[test]
    fn strings_with_hashes_and_escapes() {
        let cfg = parse("[site]\nname = \"a # b \\\"q\\\"\"").unwrap();
        assert_eq!(cfg.name, "a # b \"q\"");
    }

    #[test]
    fn parses_site_theme_table() {
        let cfg = parse(
            r##"
[site]
name = "finance-query"

[site.theme]
primary = "#BF360C"
secondary = "#5B6270"
"##,
        )
        .unwrap();
        assert_eq!(cfg.theme.primary.as_deref(), Some("#BF360C"));
        assert_eq!(cfg.theme.secondary.as_deref(), Some("#5B6270"));
        assert_eq!(cfg.theme.tertiary, None);
        assert!(!cfg.theme.is_default());
    }

    #[test]
    fn site_theme_defaults_to_unset() {
        let cfg = parse("[site]\nname = \"x\"").unwrap();
        assert!(cfg.theme.is_default());
    }

    #[test]
    fn rejects_bad_hex_in_site_theme() {
        assert!(parse("[site.theme]\nprimary = \"not-a-color\"").is_err());
    }

    #[test]
    fn rejects_unknown_site_theme_key() {
        assert!(parse("[site.theme]\nquaternary = \"#FFFFFF\"").is_err());
    }
}
