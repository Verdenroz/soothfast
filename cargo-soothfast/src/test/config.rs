//! The `[test]` section of `soothfast.toml`.
//!
//! `soothfast.toml` is shared with the site and spec engines, so this parser
//! skips every table it doesn't own.

use std::collections::BTreeMap;
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
    /// `ci = false` drops the test job of the reusable workflow.
    pub ci: Option<bool>,
    /// Allow-lists by package name, from `[test.packages.NAME]` tables.
    pub packages: BTreeMap<String, AllowList>,
}

/// The only files of one package that select it or enter its key.
#[derive(Default, Debug, PartialEq)]
pub struct AllowList {
    /// The table header as written, `test.packages.NAME`, for messages.
    pub header: String,
    /// Globs relative to the package's manifest dir.
    pub files: Vec<String>,
}

/// The table a line of `soothfast.toml` sits in, as far as `[test]` cares.
enum Table {
    Test,
    Package(String),
    Other,
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

/// Parse the `[test]` section of a `soothfast.toml`, with its
/// `[test.packages.NAME]` tables.
pub fn parse(text: &str) -> Result<TestConfig, String> {
    let mut cfg = TestConfig::default();
    let mut table = Table::Other;
    let mut opened: Vec<(usize, String)> = Vec::new();

    for (lineno, line) in logical_lines(text) {
        let line = line.as_str();
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            table =
                open_table(&mut cfg, inner.trim()).map_err(|e| format!("line {lineno}: {e}"))?;
            if let Table::Package(name) = &table {
                opened.push((lineno, name.clone()));
            }
            continue;
        }
        if let Table::Other = table {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {lineno}: expected `key = value`"));
        };
        let value = parse_value(value.trim()).map_err(|e| format!("line {lineno}: {e}"))?;
        let set = match &table {
            Table::Test => set(&mut cfg, key.trim(), value),
            Table::Package(name) => set_files(&mut cfg.packages, name, key.trim(), value),
            Table::Other => Ok(()),
        };
        set.map_err(|e| format!("line {lineno}: {e}"))?;
    }
    for (lineno, name) in opened {
        let list = &cfg.packages[&name];
        if list.files.is_empty() {
            return Err(format!(
                "line {lineno}: [{}] has no `files`, so it would ignore every file of the package; remove the table",
                list.header
            ));
        }
    }
    Ok(cfg)
}

fn open_table(cfg: &mut TestConfig, header: &str) -> Result<Table, String> {
    if header == "test" {
        return Ok(Table::Test);
    }
    let Some(rest) = header.strip_prefix("test.") else {
        return Ok(Table::Other);
    };
    let name = rest
        .strip_prefix("packages.")
        .map(|n| match n.strip_prefix('"') {
            Some(quoted) => quoted.strip_suffix('"').unwrap_or("\""),
            None => n,
        })
        .filter(|n| !n.is_empty() && !n.contains(['.', '"']))
        .ok_or_else(|| format!("unknown table [{header}]; expected [test.packages.NAME]"))?;
    if cfg.packages.contains_key(name) {
        return Err(format!("[{header}] appears twice"));
    }
    let list = AllowList {
        header: header.to_string(),
        files: Vec::new(),
    };
    cfg.packages.insert(name.to_string(), list);
    Ok(Table::Package(name.to_string()))
}

fn set_files(
    packages: &mut BTreeMap<String, AllowList>,
    name: &str,
    key: &str,
    value: TomlValue,
) -> Result<(), String> {
    let Some(list) = packages.get_mut(name) else {
        return Err(format!("`{key}` outside a [test.packages.NAME] table"));
    };
    match (key, value) {
        ("files", TomlValue::StrArray(v)) => list.files = v,
        (k, _) => return Err(format!("unknown or mistyped `{k}` under [{}]", list.header)),
    }
    Ok(())
}

impl TestConfig {
    /// Fail on an allow-list for a package the workspace does not have.
    pub fn check_packages<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), String> {
        let names: Vec<&str> = names.into_iter().collect();
        match self
            .packages
            .iter()
            .find(|(name, _)| !names.contains(&name.as_str()))
        {
            Some((name, list)) => Err(format!(
                "soothfast.toml [{}]: no workspace package named `{name}`",
                list.header
            )),
            None => Ok(()),
        }
    }
}

fn set(cfg: &mut TestConfig, key: &str, value: TomlValue) -> Result<(), String> {
    match (key, value) {
        ("ignore", TomlValue::StrArray(v)) => cfg.ignore = v,
        ("inputs", TomlValue::StrArray(v)) => cfg.inputs = v,
        ("env", TomlValue::StrArray(v)) => cfg.env = v,
        ("ci", TomlValue::Bool(b)) => cfg.ci = Some(b),
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
    fn suite_tables_belong_to_the_project_reader() {
        let text = "[[test.suite]]\nlabel = \"x\"\ncmd = \"y\"\n\n[test]\nci = false\n";
        assert_eq!(parse(text).unwrap().ci, Some(false));
    }

    #[test]
    fn reads_the_ci_switch() {
        assert_eq!(parse("[test]\nci = false\n").unwrap().ci, Some(false));
        assert!(parse("[test]\nci = \"no\"\n").is_err());
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

    #[test]
    fn reads_package_allow_lists_with_bare_or_quoted_names() {
        let text = "[test]\nignore = [\"*.md\"]\n\n[test.packages.finance-query]\nfiles = [\"src/**\", \"build.rs\"]\n\n[test.packages.\"fq-cli\"]\nfiles = [\"src/**\"]\n\n[site]\nname = \"x\"\n";
        let cfg = parse(text).unwrap();
        assert_eq!(cfg.ignore, ["*.md"]);
        assert_eq!(cfg.packages["finance-query"].files, ["src/**", "build.rs"]);
        assert_eq!(
            cfg.packages["finance-query"].header,
            "test.packages.finance-query"
        );
        assert_eq!(cfg.packages["fq-cli"].header, "test.packages.\"fq-cli\"");
    }

    #[test]
    fn a_bad_package_table_is_an_error_naming_it_as_written() {
        for (text, want) in [
            (
                "[test.packages.\"a\"]\nfiles = []\n",
                "[test.packages.\"a\"] has no `files`",
            ),
            ("[test.packages.a]\n", "[test.packages.a] has no `files`"),
            (
                "[test.packages.a]\nfiles = [\"x\"]\n[test.packages.a]\nfiles = [\"y\"]\n",
                "[test.packages.a] appears twice",
            ),
            (
                "[test.packages.a]\nfiles = [\"x\"]\nignore = []\n",
                "`ignore` under [test.packages.a]",
            ),
            (
                "[test.packages.a]\nfiles = \"x\"\n",
                "`files` under [test.packages.a]",
            ),
            ("[test.other]\nx = 1\n", "unknown table [test.other]"),
            (
                "[test.packages.a.b]\nfiles = [\"x\"]\n",
                "unknown table [test.packages.a.b]",
            ),
            (
                "[test.packages.\"a]\nfiles = [\"x\"]\n",
                "unknown table [test.packages.\"a]",
            ),
            (
                "[test.packages.a\"]\nfiles = [\"x\"]\n",
                "unknown table [test.packages.a\"]",
            ),
            (
                "x = 1\n\n[test.packages.a]\n",
                "line 3: [test.packages.a] has no `files`",
            ),
        ] {
            let e = parse(text).unwrap_err();
            assert!(e.contains(want), "{text:?}: {e}");
        }
    }

    #[test]
    fn an_allow_list_for_a_missing_package_is_an_error() {
        let cfg = parse("[test.packages.\"gone\"]\nfiles = [\"src/**\"]\n").unwrap();
        assert_eq!(
            cfg.check_packages(["a", "b"]).unwrap_err(),
            "soothfast.toml [test.packages.\"gone\"]: no workspace package named `gone`"
        );
        assert!(cfg.check_packages(["gone"]).is_ok());
    }
}
