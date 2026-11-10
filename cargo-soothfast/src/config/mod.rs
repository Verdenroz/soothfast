//! The root `soothfast.toml`, read once and resolved per section.
//!
//! The file is shared with the site and spec engines, so each section parser
//! skips every table it doesn't own.

use std::path::{Path, PathBuf};

pub mod changelog;
pub mod docs;
pub mod gate;
pub mod package;

pub use package::{Group, Settings};

use crate::test::config as test;

/// A root `soothfast.toml`'s text. Sections parse on demand, so a malformed
/// table only fails the commands that read it.
pub struct Config {
    path: PathBuf,
    text: String,
}

impl Config {
    /// Read `soothfast.toml` from a directory. An absent file reads as empty,
    /// so every section is its default.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join("soothfast.toml");
        let text = match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
            Ok(text) => text,
        };
        Ok(Self { path, text })
    }

    /// The `[gate]` section.
    pub fn gate(&self) -> Result<gate::GateConfig, String> {
        gate::parse(&self.text).map_err(|e| self.located(e))
    }

    /// The `[docs]` section.
    pub fn docs(&self) -> Result<docs::DocsConfig, String> {
        docs::parse(&self.text).map_err(|e| self.located(e))
    }

    /// The `[changelog]` and `[changelog.icons]` tables.
    pub fn changelog(&self) -> Result<changelog::ChangelogConfig, String> {
        changelog::parse(&self.text).map_err(|e| self.located(e))
    }

    /// The `[test]` section and its `[test.packages.NAME]` tables.
    pub fn test(&self) -> Result<test::TestConfig, String> {
        test::parse(&self.text).map_err(|e| self.located(e))
    }

    /// What the flags of `group`'s commands default to for `pkgs`: the
    /// package's group table, then its own table, then the repo-wide section.
    /// Packages the file doesn't list resolve to the repo-wide defaults. A
    /// set of packages that resolve differently has no single default.
    pub fn resolve(&self, pkgs: &[String], group: Group) -> Result<Settings, String> {
        let tables = package::parse(&self.text).map_err(|e| self.located(e))?;
        let mut each = pkgs.iter().map(|p| {
            let settings = tables.get(p).map(|t| t.settings(group));
            (p, settings.unwrap_or_default())
        });
        let first = each.next();
        if let Some((name, first)) = &first
            && let Some((other, _)) = each.find(|(_, s)| s != first)
        {
            return Err(self.located(format!(
                "packages {name} and {other} resolve different {group} defaults; pass the flags"
            )));
        }
        let own = first.map(|(_, s)| s).unwrap_or_default();
        Ok(own.or(self.repo_wide(group)?))
    }

    fn repo_wide(&self, group: Group) -> Result<Settings, String> {
        Ok(match group {
            Group::Gate | Group::Spec => Settings {
                features: self.gate()?.features,
                ..Settings::default()
            },
            Group::Docs => Settings {
                capture: Some(self.docs()?.paths),
                ..Settings::default()
            },
            Group::Test => Settings::default(),
        })
    }

    fn located(&self, e: String) -> String {
        format!("{}: {e}", self.path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        Config {
            path: PathBuf::from("soothfast.toml"),
            text: text.into(),
        }
    }

    #[test]
    fn an_absent_file_is_every_default() {
        let dir = crate::test::fixture::TempDir::new("config-absent");
        let cfg = Config::load(&dir).unwrap();
        assert_eq!(cfg.gate().unwrap(), gate::GateConfig::default());
        assert_eq!(cfg.docs().unwrap(), docs::DocsConfig::default());
        assert_eq!(cfg.test().unwrap(), test::TestConfig::default());
    }

    #[test]
    fn sections_parse_from_one_text() {
        let cfg = config("[gate]\nfeatures = \"a\"\n\n[docs]\npaths = [\"x\"]\n");
        assert_eq!(cfg.gate().unwrap().features.as_deref(), Some("a"));
        assert_eq!(cfg.docs().unwrap().paths, ["x"]);
    }

    #[test]
    fn a_malformed_section_only_fails_its_reader() {
        let cfg = config("[gate]\nfeatures = 3\n\n[docs]\npaths = [\"x\"]\n");
        assert!(cfg.docs().is_ok());
        let e = cfg.gate().unwrap_err();
        assert!(e.starts_with("soothfast.toml: line 2"), "{e}");
    }

    fn pkgs(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    const TEXT: &str = "[gate]\nfeatures = \"repo\"\n\n[package.core]\nfeatures = \"all\"\ntarget = \"t\"\n\n[package.core.gate]\nfeatures = \"bench\"\n\n[package.core.docs]\nbaseline = \"main\"\n\n[package.other]\nfeatures = \"x\"\n";

    #[test]
    fn precedence_is_group_then_package_then_repo_wide() {
        let cfg = config(TEXT);
        let gate = cfg.resolve(&pkgs(&["core"]), Group::Gate).unwrap();
        assert_eq!(gate.features.as_deref(), Some("bench"));
        assert_eq!(gate.target.as_deref(), Some("t"));
        let docs = cfg.resolve(&pkgs(&["core"]), Group::Docs).unwrap();
        assert_eq!(docs.features.as_deref(), Some("all"));
        assert_eq!(docs.baseline.as_deref(), Some("main"));
        let spec = cfg.resolve(&pkgs(&["unlisted"]), Group::Spec).unwrap();
        assert_eq!(spec.features.as_deref(), Some("repo"));
    }

    #[test]
    fn repo_wide_docs_paths_are_the_capture_default() {
        let cfg = config("[docs]\npaths = [\"book\"]\n");
        let docs = cfg.resolve(&[], Group::Docs).unwrap();
        assert_eq!(docs.capture, Some(vec!["book".into()]));
    }

    #[test]
    fn packages_that_resolve_differently_have_no_default() {
        let e = config(TEXT)
            .resolve(&pkgs(&["core", "other"]), Group::Docs)
            .unwrap_err();
        assert!(e.contains("core and other") && e.contains("docs"), "{e}");
        let same = config(TEXT).resolve(&pkgs(&["core", "core"]), Group::Docs);
        assert!(same.is_ok());
    }

    #[test]
    fn no_package_and_no_file_resolves_to_nothing() {
        let cfg = config("");
        let settings = cfg.resolve(&[], Group::Gate).unwrap();
        assert_eq!(settings, Settings::default());
    }
}
