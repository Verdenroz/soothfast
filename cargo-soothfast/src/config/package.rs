//! The `[package.NAME]` and `[package.NAME.GROUP]` tables of `soothfast.toml`.
//!
//! A package table holds defaults for one workspace package. A group table
//! overrides them for the commands of that group. The set of package names
//! is whatever the file lists; nothing checks them against the workspace.

use std::collections::BTreeMap;

use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

/// The command groups a package's defaults can differ across.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    /// `gate`, `measure`, `baseline`, `prebuild`, `trend append`, `buildcost`.
    Gate,
    /// `docs *`, `coverage`, `report`, `mcp`.
    Docs,
    /// `spec *` and `sdk *`.
    Spec,
    /// `test`.
    Test,
}

impl std::fmt::Display for Group {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Group::Gate => "gate",
            Group::Docs => "docs",
            Group::Spec => "spec",
            Group::Test => "test",
        })
    }
}

impl Group {
    fn parse(name: &str) -> Option<Group> {
        match name {
            "gate" => Some(Group::Gate),
            "docs" => Some(Group::Docs),
            "spec" => Some(Group::Spec),
            "test" => Some(Group::Test),
            _ => None,
        }
    }

    /// Whether the group's commands have a flag for `key`.
    fn takes(self, key: &str) -> bool {
        match key {
            "features" => true,
            "target" => self != Group::Test,
            "baseline" => matches!(self, Group::Gate | Group::Docs),
            "paths" | "coverage" => self == Group::Docs,
            _ => false,
        }
    }
}

/// Defaults for what a command's flags would set. `None` leaves the flag's
/// own default in force.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Cargo features, in the form `--features` takes.
    pub features: Option<String>,
    /// The bench target a command builds and runs, as `--bench` takes it. Only
    /// a group table sets it, and `test` has none.
    pub target: Option<String>,
    /// The baseline a command compares against or saves to.
    pub baseline: Option<String>,
    /// The PATHS `docs` commands scan when none are given, in place of
    /// `[docs] paths`.
    pub paths: Option<Vec<String>>,
    /// The minimum percent `coverage docs` requires, as `--min` takes it.
    pub coverage: Option<u32>,
}

impl Settings {
    /// These settings, with `lower` supplying every key they leave unset.
    pub fn or(self, lower: Settings) -> Settings {
        Settings {
            features: self.features.or(lower.features),
            target: self.target.or(lower.target),
            baseline: self.baseline.or(lower.baseline),
            paths: self.paths.or(lower.paths),
            coverage: self.coverage.or(lower.coverage),
        }
    }
}

/// One package's table and its group overrides.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct PackageTable {
    pub base: Settings,
    pub groups: BTreeMap<Group, Settings>,
}

impl PackageTable {
    /// The group's overrides over the package's own defaults.
    pub fn settings(&self, group: Group) -> Settings {
        self.groups
            .get(&group)
            .cloned()
            .unwrap_or_default()
            .or(self.base.clone())
    }
}

/// Where a line sits: the package and group of the table it is under.
struct Open {
    name: String,
    group: Option<Group>,
    header: String,
}

/// Parse the package tables of a `soothfast.toml`, keyed by package name.
pub fn parse(text: &str) -> Result<BTreeMap<String, PackageTable>, String> {
    let mut packages: BTreeMap<String, PackageTable> = BTreeMap::new();
    let mut open: Option<Open> = None;

    for (lineno, line) in logical_lines(text) {
        let line = line.as_str();
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            open = open_table(&mut packages, inner.trim())
                .map_err(|e| format!("line {lineno}: {e}"))?;
            continue;
        }
        let Some(open) = &open else { continue };

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {lineno}: expected `key = value`"));
        };
        let value = parse_value(value.trim()).map_err(|e| format!("line {lineno}: {e}"))?;
        let table = packages.entry(open.name.clone()).or_default();
        let settings = match open.group {
            Some(group) => table.groups.entry(group).or_default(),
            None => &mut table.base,
        };
        set(settings, open.group, key.trim(), value).map_err(|_| {
            format!(
                "line {lineno}: unknown or mistyped `{}` under [{}]",
                key.trim(),
                open.header
            )
        })?;
    }
    Ok(packages)
}

fn open_table(
    packages: &mut BTreeMap<String, PackageTable>,
    header: &str,
) -> Result<Option<Open>, String> {
    let Some(rest) = header.strip_prefix("package.") else {
        return Ok(None);
    };
    let bad = || {
        format!(
            "unknown table [{header}]; expected [package.NAME] or [package.NAME.gate|docs|spec|test]"
        )
    };
    let (name, group) = match rest.strip_prefix('"') {
        Some(quoted) => {
            let (name, after) = quoted.split_once('"').ok_or_else(bad)?;
            (name, after.strip_prefix('.'))
        }
        None => match rest.split_once('.') {
            Some((name, group)) => (name, Some(group)),
            None => (rest, None),
        },
    };
    if name.is_empty() || name.contains('"') {
        return Err(bad());
    }
    let group = group.map(|g| Group::parse(g).ok_or_else(bad)).transpose()?;
    let table = packages.entry(name.to_string()).or_default();
    let seen = match group {
        Some(g) => table.groups.contains_key(&g),
        None => table.base != Settings::default(),
    };
    if seen {
        return Err(format!("[{header}] appears twice"));
    }
    if let Some(g) = group {
        table.groups.insert(g, Settings::default());
    }
    Ok(Some(Open {
        name: name.to_string(),
        group,
        header: header.to_string(),
    }))
}

fn set(
    settings: &mut Settings,
    group: Option<Group>,
    key: &str,
    value: TomlValue,
) -> Result<(), ()> {
    if group.is_some_and(|g| !g.takes(key)) {
        return Err(());
    }
    match (key, value) {
        ("features", TomlValue::Str(s)) => settings.features = Some(s),
        ("target", TomlValue::Str(s)) if group.is_some() => settings.target = Some(s),
        ("baseline", TomlValue::Str(s)) => settings.baseline = Some(s),
        ("paths", TomlValue::StrArray(v)) => settings.paths = Some(v),
        ("coverage", TomlValue::Int(n)) if group.is_some() => {
            settings.coverage = Some(u32::try_from(n).ok().filter(|n| *n <= 100).ok_or(())?)
        }
        _ => return Err(()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_package_and_its_groups() {
        let text = "[package.core]\nfeatures = \"a\"\nbaseline = \"main\"\n\n[package.core.gate]\nfeatures = \"bench\"\n\n[package.core.docs]\npaths = [\"book/**\"]\n";
        let core = &parse(text).unwrap()["core"];
        assert_eq!(core.base.features.as_deref(), Some("a"));
        assert_eq!(core.groups[&Group::Gate].features.as_deref(), Some("bench"));
        assert_eq!(
            core.groups[&Group::Docs].paths,
            Some(vec!["book/**".into()])
        );
    }

    #[test]
    fn a_group_overrides_the_package_key_by_key() {
        let text = "[package.core]\nfeatures = \"a\"\nbaseline = \"b\"\n\n[package.core.gate]\nfeatures = \"bench\"\ntarget = \"t\"\n";
        let core = &parse(text).unwrap()["core"];
        let gate = core.settings(Group::Gate);
        assert_eq!(gate.features.as_deref(), Some("bench"));
        assert_eq!(gate.baseline.as_deref(), Some("b"));
        assert_eq!(gate.target.as_deref(), Some("t"));
        assert_eq!(core.settings(Group::Docs).target, None);
        assert_eq!(core.settings(Group::Docs).features.as_deref(), Some("a"));
    }

    #[test]
    fn any_package_name_is_accepted_quoted_or_bare() {
        let text =
            "[package.my-crate]\nfeatures = \"a\"\n\n[package.\"b.c\".test]\nfeatures = \"b\"\n";
        let packages = parse(text).unwrap();
        assert!(packages.contains_key("my-crate") && packages.contains_key("b.c"));
    }

    #[test]
    fn other_tables_are_skipped() {
        let text = "[gate]\nfeatures = \"x\"\n\n[docs]\npackage = \"a\"\n\n[test.packages.a]\nfiles = [\"x\"]\n\n[[spec]]\npath = \"y\"\n";
        assert!(parse(text).unwrap().is_empty());
    }

    #[test]
    fn a_mistyped_key_names_its_line_and_table() {
        let e = parse("[package.a]\nfeatures = 3\n").unwrap_err();
        assert_eq!(
            e,
            "line 2: unknown or mistyped `features` under [package.a]"
        );
    }

    #[test]
    fn a_key_the_group_has_no_flag_for_is_an_error() {
        assert!(parse("[package.a.spec]\nbaseline = \"b\"\n").is_err());
        assert!(parse("[package.a.gate]\npaths = [\"x\"]\n").is_err());
        assert!(parse("[package.a.test]\nbaseline = \"b\"\n").is_err());
    }

    #[test]
    fn target_belongs_to_a_group_table() {
        let e = parse("[package.a]\ntarget = \"t\"\n").unwrap_err();
        assert_eq!(e, "line 2: unknown or mistyped `target` under [package.a]");
    }

    #[test]
    fn the_test_group_has_no_target() {
        assert!(parse("[package.a.test]\ntarget = \"t\"\n").is_err());
        assert!(parse("[package.a.test]\nfeatures = \"f\"\n").is_ok());
    }

    #[test]
    fn coverage_is_a_percent_of_the_docs_group_only() {
        let docs = parse("[package.a.docs]\ncoverage = 85\n").unwrap();
        assert_eq!(docs["a"].groups[&Group::Docs].coverage, Some(85));
        assert!(parse("[package.a.gate]\ncoverage = 85\n").is_err());
        assert!(parse("[package.a]\ncoverage = 85\n").is_err());
        assert!(parse("[package.a.docs]\ncoverage = 101\n").is_err());
        assert!(parse("[package.a.docs]\ncoverage = \"85\"\n").is_err());
    }

    #[test]
    fn an_unknown_group_is_an_error() {
        let e = parse("[package.a.lint]\n").unwrap_err();
        assert!(e.contains("expected [package.NAME]"), "{e}");
    }

    #[test]
    fn a_table_written_twice_is_an_error() {
        assert!(parse("[package.a.gate]\n[package.a.gate]\n").is_err());
        assert!(parse("[package.a]\nfeatures = \"x\"\n[package.a]\n").is_err());
    }
}
