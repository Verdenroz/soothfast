//! The `[docs]` table of `soothfast.toml`.
//!
//! `soothfast.toml` is shared with the site, spec and gate engines, so this
//! parser skips every table it doesn't own.

use std::path::Path;

use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

/// Repo-level `docs` settings. PATHS on the command line override them.
#[derive(Debug, PartialEq, Eq)]
pub struct DocsConfig {
    /// Files, directories and globs (relative to the project root) whose
    /// markdown carries the scanned markers.
    pub paths: Vec<String>,
    /// Workspace packages `docs check` reads, when named. `None` leaves the
    /// choice to whoever runs it.
    pub package: Option<Vec<String>>,
}

impl Default for DocsConfig {
    fn default() -> Self {
        Self {
            paths: vec!["README.md".into(), "docs".into()],
            package: None,
        }
    }
}

/// Read `[docs]` from a directory's `soothfast.toml`. An absent file just
/// means defaults.
pub fn load(dir: &Path) -> Result<DocsConfig, String> {
    let path = dir.join("soothfast.toml");
    match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocsConfig::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
    }
}

/// Parse the `[docs]` table of a `soothfast.toml`.
pub fn parse(text: &str) -> Result<DocsConfig, String> {
    let mut cfg = DocsConfig::default();
    let mut in_docs = false;

    for (lineno, line) in logical_lines(text) {
        let line = line.as_str();
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_docs = inner.trim() == "docs";
            continue;
        }
        if !in_docs {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {lineno}: expected `key = value`"));
        };
        let value = parse_value(value.trim()).map_err(|e| format!("line {lineno}: {e}"))?;
        match (key.trim(), value) {
            ("paths", TomlValue::StrArray(v)) => cfg.paths = v,
            ("package", TomlValue::Str(name)) => cfg.package = Some(vec![name]),
            ("package", TomlValue::StrArray(names)) if !names.is_empty() => {
                cfg.package = Some(names)
            }
            (k, _) => {
                return Err(format!(
                    "line {lineno}: unknown or mistyped `{k}` under [docs]"
                ));
            }
        }
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_readme_and_docs() {
        let cfg = parse("[site]\nname = \"x\"\n").unwrap();
        assert_eq!(cfg.paths, ["README.md", "docs"]);
    }

    #[test]
    fn reads_paths_and_ignores_other_tables() {
        let text = "[gate]\nfeatures = \"a\"\n\n[docs]\npaths = [\"README.md\", \"book/**\"]\n";
        assert_eq!(parse(text).unwrap().paths, ["README.md", "book/**"]);
    }

    #[test]
    fn package_is_a_name_or_a_non_empty_list_beside_paths() {
        let one = parse("[docs]\npackage = \"a\"\npaths = [\"x\"]\n").unwrap();
        assert_eq!(
            (one.package, one.paths),
            (Some(vec!["a".into()]), vec!["x".into()])
        );
        let many = parse("[docs]\npackage = [\"a\", \"b\"]\n").unwrap();
        assert_eq!(many.package, Some(vec!["a".into(), "b".into()]));
        assert_eq!(parse("[site]\n").unwrap().package, None);
        assert!(parse("[docs]\npackage = []\n").is_err());
    }

    #[test]
    fn an_unknown_key_is_an_error_not_a_silent_default() {
        let e = parse("[docs]\npath = [\"x\"]\n").unwrap_err();
        assert!(e.contains("under [docs]"), "{e}");
    }
}
