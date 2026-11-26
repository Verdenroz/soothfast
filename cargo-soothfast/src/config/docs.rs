//! The `[docs]` table of `soothfast.toml`.
//!
//! `soothfast.toml` is shared with the site, spec and gate engines, so this
//! parser skips every table it doesn't own.

use std::collections::BTreeMap;

use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

use crate::coverage_report::is_dimension;

/// The dimension and percent a `coverage = N` or `coverage_min.DIMENSION = N`
/// line sets. `None` for any other line, and for a dimension or value that
/// does not exist.
pub(super) fn coverage_min(key: &str, value: &TomlValue) -> Option<(String, u32)> {
    let name = match key {
        "coverage" => "documented",
        _ => key.strip_prefix("coverage_min.")?.trim(),
    };
    match value {
        TomlValue::Int(n) if is_dimension(name) && (0..=100).contains(n) => {
            Some((name.to_string(), *n as u32))
        }
        _ => None,
    }
}

/// Notes the `documented` minimum a line sets, and refuses a different one
/// than an earlier line of the same table set: `coverage` and
/// `coverage_min.documented` are one key.
pub(super) fn claim_documented(
    seen: &mut Option<(usize, u32)>,
    lineno: usize,
    n: u32,
) -> Result<(), String> {
    match *seen {
        Some((line, prior)) if prior != n => Err(format!(
            "line {lineno}: documented minimum {n} conflicts with {prior} on line {line}"
        )),
        _ => {
            *seen = Some((lineno, n));
            Ok(())
        }
    }
}

/// Repo-level `docs` settings. PATHS on the command line override them.
#[derive(Debug, PartialEq, Eq)]
pub struct DocsConfig {
    /// Files, directories and globs (relative to the project root) whose
    /// markdown carries the scanned markers.
    pub paths: Vec<String>,
    /// Workspace packages `docs check` reads, when named. `None` leaves the
    /// choice to whoever runs it.
    pub package: Option<Vec<String>>,
    /// The minimum percent `coverage docs` requires when `--min` is absent.
    pub coverage: Option<u32>,
    /// The minimum percent of each other coverage dimension, by name.
    /// `coverage_min.documented` is `coverage`.
    pub minimums: BTreeMap<String, u32>,
}

impl Default for DocsConfig {
    fn default() -> Self {
        Self {
            paths: vec!["README.md".into(), "docs".into()],
            package: None,
            coverage: None,
            minimums: BTreeMap::new(),
        }
    }
}

/// Parse the `[docs]` table of a `soothfast.toml`.
pub fn parse(text: &str) -> Result<DocsConfig, String> {
    let mut cfg = DocsConfig::default();
    let mut in_docs = false;
    let mut documented = None;

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
        if let Some((dimension, n)) = coverage_min(key.trim(), &value) {
            if dimension == "documented" {
                claim_documented(&mut documented, lineno, n)?;
                cfg.coverage = Some(n);
            } else {
                cfg.minimums.insert(dimension, n);
            }
            continue;
        }
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
    fn coverage_is_a_percent() {
        assert_eq!(parse("[docs]\ncoverage = 90\n").unwrap().coverage, Some(90));
        assert_eq!(parse("[site]\n").unwrap().coverage, None);
        assert!(parse("[docs]\ncoverage = 101\n").is_err());
        assert!(parse("[docs]\ncoverage = -1\n").is_err());
        assert!(parse("[docs]\ncoverage = \"90\"\n").is_err());
    }

    #[test]
    fn a_dimension_minimum_is_a_percent_of_a_known_dimension() {
        let cfg =
            parse("[docs]\ncoverage_min.documented_strict = 60\ncoverage_min.documented = 90\n")
                .unwrap();
        assert_eq!(cfg.coverage, Some(90));
        assert_eq!(cfg.minimums["documented_strict"], 60);
        assert!(!cfg.minimums.contains_key("documented"));
        assert!(parse("[docs]\ncoverage_min.docs = 60\n").is_err());
        assert!(parse("[docs]\ncoverage_min.gated = 60\n").is_ok());
        assert!(parse("[docs]\ncoverage_min.documented = 101\n").is_err());
        assert!(parse("[docs]\ncoverage_min.documented = \"9\"\n").is_err());
    }

    #[test]
    fn coverage_and_the_documented_minimum_may_not_disagree() {
        let same = "[docs]\ncoverage = 90\ncoverage_min.documented = 90\n";
        assert_eq!(parse(same).unwrap().coverage, Some(90));
        let e = parse("[docs]\ncoverage = 90\ncoverage_min.documented = 80\n").unwrap_err();
        assert_eq!(
            e,
            "line 3: documented minimum 80 conflicts with 90 on line 2"
        );
    }

    #[test]
    fn an_unknown_key_is_an_error_not_a_silent_default() {
        let e = parse("[docs]\npath = [\"x\"]\n").unwrap_err();
        assert!(e.contains("under [docs]"), "{e}");
    }
}
