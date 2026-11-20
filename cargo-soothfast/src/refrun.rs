//! Where `report changelog` gets the measurement of the release it compares
//! against: HEAD's own baseline, a stored report, a run the gate recorded, or
//! a worktree build.

use std::path::Path;

use crate::config::{Config, Group};
use crate::invoke::{self, CommonArgs};

/// The commit a reference measurement stands for, relative to HEAD.
#[derive(Debug, PartialEq)]
pub enum Anchor {
    /// The reference is HEAD itself.
    Head,
    /// The merge-base of HEAD and the reference; empty when git has none.
    At(String),
}

/// The merge-base of HEAD and `refname` in the repository at `dir`.
pub fn anchor(dir: &Path, refname: &str) -> Anchor {
    let sha = invoke::git_in(dir, &["merge-base", "HEAD", refname]).unwrap_or_default();
    match invoke::git_in(dir, &["rev-parse", "HEAD"]) {
        Ok(head) if head == sha => Anchor::Head,
        _ => Anchor::At(sha),
    }
}

/// The flags the release is measured under: `pkg`'s `gate` defaults, which
/// `measure` filled in for the baseline the measurement is compared with.
pub fn common(config: &Config, pkg: &str) -> Result<CommonArgs, String> {
    let mut common = CommonArgs {
        pkg: Some(pkg.to_string()),
        ..CommonArgs::default()
    };
    config.fill(
        &mut common,
        Group::Gate,
        std::slice::from_ref(&pkg.to_string()),
    )?;
    Ok(common)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{Anchor, anchor, common, invoke};
    use crate::config::Config;
    use crate::runcache;
    use crate::test::fixture::TempDir;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn repo(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("soothfast-refrun-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "one"]);
        dir
    }

    #[test]
    fn a_tag_on_head_anchors_at_head() {
        let dir = repo("at-head");
        git(&dir, &["tag", "v1"]);
        assert_eq!(anchor(&dir, "v1"), Anchor::Head);
    }

    #[test]
    fn a_tag_behind_head_anchors_at_its_commit() {
        let dir = repo("behind");
        git(&dir, &["tag", "v1"]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "two"]);
        let tagged = invoke::git_in(&dir, &["rev-parse", "v1"]).unwrap();
        assert_eq!(anchor(&dir, "v1"), Anchor::At(tagged));
    }

    fn configured(name: &str, text: &str) -> Config {
        let dir = TempDir::new(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("soothfast.toml"), text).unwrap();
        Config::load(&dir).unwrap()
    }

    const TWO_GROUPS: &str = "[package.demo.docs]\nfeatures = \"full\"\n\n\
        [package.demo.gate]\nfeatures = \"bench\"\ntarget = \"t\"\n";

    #[test]
    fn the_release_is_measured_under_the_gate_groups_features() {
        let cfg = configured("refrun-gate-group", TWO_GROUPS);
        let measured = common(&cfg, "demo").unwrap();
        assert_eq!(measured.features.as_deref(), Some("bench"));
        assert_eq!(measured.target.as_deref(), Some("t"));
        assert_eq!(measured.pkg.as_deref(), Some("demo"));
    }

    #[test]
    fn the_report_key_follows_the_gate_groups_features() {
        let cfg = configured("refrun-gate-key", TWO_GROUPS);
        let stamp = crate::buildstamp::BuildStamp {
            rustc: "1.88.0".into(),
            codegen_units: "1".into(),
            profiles: "p".into(),
            rustflags: "f".into(),
        };
        let key = |features: Option<&str>| runcache::report_key("aaa", "demo", features, &stamp);
        let measured = common(&cfg, "demo").unwrap();
        assert_eq!(key(measured.features.as_deref()), key(Some("bench")));
        assert_ne!(key(measured.features.as_deref()), key(Some("full")));
    }

    #[test]
    fn the_gates_codegen_units_pin_reaches_the_release_build() {
        let cfg = configured("refrun-codegen", "[gate]\ncodegen-units = 4\n");
        let measured = common(&cfg, "demo").unwrap();
        assert_eq!(measured.codegen_units.as_deref(), Some("4"));
    }
}
