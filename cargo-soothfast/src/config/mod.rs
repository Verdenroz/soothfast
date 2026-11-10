//! The root `soothfast.toml`, read once and resolved per section.
//!
//! The file is shared with the site and spec engines, so each section parser
//! skips every table it doesn't own.

use std::path::{Path, PathBuf};

pub mod changelog;
pub mod ci;
pub mod docs;
pub mod gate;
pub mod package;

pub use package::{Group, Settings};

use crate::invoke::{self, CommonArgs};
use crate::test::config as test;

/// What a command read from `soothfast.toml` beyond the flags it filled.
#[derive(Default)]
pub struct Applied {
    pub gate: gate::GateConfig,
    pub settings: Settings,
}

/// Fill the flags `common` leaves unset from the defaults of its `-p`
/// package in `group`, and return what was read. Without a workspace there is
/// no file and nothing to fill.
pub fn apply(common: &mut CommonArgs, group: Group) -> Result<Applied, String> {
    let pkgs: Vec<String> = common.pkg.iter().cloned().collect();
    apply_to(common, group, &pkgs)
}

/// [`apply`] for a command that works on `pkgs` together.
pub fn apply_to(common: &mut CommonArgs, group: Group, pkgs: &[String]) -> Result<Applied, String> {
    let Ok(root) = invoke::workspace_root() else {
        return Ok(Applied::default());
    };
    Config::load(&root)?.fill(common, group, pkgs)
}

/// The defaults of `pkgs` in `group` from the project's `soothfast.toml`, for
/// a command that has no [`CommonArgs`] to fill.
pub fn defaults(pkgs: &[String], group: Group) -> Result<Settings, String> {
    Config::load(&invoke::project_root())?.resolve(pkgs, group)
}

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

    /// Fill the flags `common` leaves unset from the defaults of `pkgs` in
    /// `group`. `target` fills `--bench`, the bench target the command builds.
    pub fn fill(
        &self,
        common: &mut CommonArgs,
        group: Group,
        pkgs: &[String],
    ) -> Result<Applied, String> {
        let gate = self.gate()?;
        let settings = self.resolve(pkgs, group)?;
        if common.codegen_units.is_none() {
            common.codegen_units = gate.codegen_units.clone();
        }
        if common.features.is_none() {
            common.features = settings.features.clone();
        }
        if common.target.is_none() {
            common.target = settings.target.clone();
        }
        Ok(Applied { gate, settings })
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

    /// The `[ci]` table's `skip` list: the CI job kinds not to run.
    /// `[test] ci = false` adds `"test"` when the list doesn't name it.
    pub fn ci(&self) -> Result<Vec<String>, String> {
        let mut skip = ci::parse(&self.text).map_err(|e| self.located(e))?;
        let test_off = self.test()?.ci == Some(false);
        if test_off && !skip.iter().any(|k| k == "test") {
            skip.push("test".into());
        }
        Ok(skip)
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
                "packages {name} and {other} resolve different {group} defaults; run it once per package"
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
                paths: Some(self.docs()?.paths).filter(|p| !p.is_empty()),
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

    #[test]
    fn ci_skip_alone_and_with_the_test_switch() {
        assert!(config("").ci().unwrap().is_empty());
        assert_eq!(config("[ci]\nskip = [\"sdk\"]\n").ci().unwrap(), ["sdk"]);
        assert_eq!(config("[test]\nci = false\n").ci().unwrap(), ["test"]);
        let both = config("[test]\nci = false\n\n[ci]\nskip = [\"test\", \"sdk\"]\n");
        assert_eq!(both.ci().unwrap(), ["test", "sdk"]);
        assert!(config("[test]\nci = true\n").ci().unwrap().is_empty());
    }

    const TEXT: &str = "[gate]\nfeatures = \"repo\"\n\n[package.core]\nfeatures = \"all\"\n\n[package.core.gate]\nfeatures = \"bench\"\ntarget = \"t\"\n\n[package.core.docs]\nbaseline = \"main\"\n\n[package.other]\nfeatures = \"x\"\n";

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
    fn repo_wide_docs_paths_are_the_paths_default() {
        let cfg = config("[docs]\npaths = [\"book\"]\n");
        let docs = cfg.resolve(&[], Group::Docs).unwrap();
        assert_eq!(docs.paths, Some(vec!["book".into()]));
    }

    #[test]
    fn an_empty_docs_paths_leaves_the_scope_to_the_command() {
        let docs = config("[docs]\npaths = []\n")
            .resolve(&[], Group::Docs)
            .unwrap();
        assert_eq!(docs.paths, None);
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

    const FLAGLESS: &str = "[package.core]\nfeatures = \"all\"\nbaseline = \"main\"\n\n[package.core.gate]\nfeatures = \"bench\"\ntarget = \"perf\"\n\n[package.core.spec]\ntarget = \"routes\"\n";

    fn core() -> CommonArgs {
        CommonArgs {
            pkg: Some("core".into()),
            ..CommonArgs::default()
        }
    }

    #[test]
    fn the_gate_group_fills_features_and_the_bench_target() {
        let mut common = core();
        let applied = config(FLAGLESS)
            .fill(&mut common, Group::Gate, &pkgs(&["core"]))
            .unwrap();
        assert_eq!(common.features.as_deref(), Some("bench"));
        assert_eq!(common.target.as_deref(), Some("perf"));
        assert_eq!(applied.settings.baseline.as_deref(), Some("main"));
    }

    #[test]
    fn a_flag_beats_every_table() {
        let mut common = CommonArgs {
            features: Some("cli".into()),
            target: Some("mine".into()),
            ..core()
        };
        config(FLAGLESS)
            .fill(&mut common, Group::Gate, &pkgs(&["core"]))
            .unwrap();
        assert_eq!(common.features.as_deref(), Some("cli"));
        assert_eq!(common.target.as_deref(), Some("mine"));
    }

    #[test]
    fn the_spec_group_fills_the_bench_target() {
        let mut common = core();
        let applied = config(FLAGLESS)
            .fill(&mut common, Group::Spec, &pkgs(&["core"]))
            .unwrap();
        assert_eq!(common.features.as_deref(), Some("all"));
        assert_eq!(common.target.as_deref(), Some("routes"));
        assert_eq!(applied.settings.target.as_deref(), Some("routes"));
    }

    #[test]
    fn the_docs_group_fills_features_and_leaves_an_unset_target() {
        let mut common = core();
        config(FLAGLESS)
            .fill(&mut common, Group::Docs, &pkgs(&["core"]))
            .unwrap();
        assert_eq!(common.features.as_deref(), Some("all"));
        assert_eq!(common.target, None);
    }
}
