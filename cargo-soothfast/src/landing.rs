//! Whether a pull request is one of soothfast-bot's own landings: regenerated
//! files only, which the lanes have nothing to say about. Anything unknown
//! or unreadable makes it an ordinary pull request.

use std::collections::BTreeSet;
use std::path::{Component, Path};

use crate::init::bot::Landing;
use crate::spec_config::Mode;
use crate::{invoke, sdk_config, spec_config, workspace};

const BRANCH_PREFIX: &str = "bot/";
const CONFIG_NAME: &str = "soothfast.toml";

/// What the workflow knows about the pull request being planned.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct PullRequest {
    /// Login of whoever opened it.
    pub author: Option<String>,
    pub head_ref: Option<String>,
    /// `owner/name` of the repository the head branch is in.
    pub head_repo: Option<String>,
    /// `owner/name` of the repository the pull request targets.
    pub repo: Option<String>,
    /// Whether GitHub created and signed every commit on it for its author,
    /// which a push cannot do.
    pub verified_commits: bool,
}

impl PullRequest {
    /// Whether the bot with App slug `slug` opened this from a `bot/` branch
    /// of the repository itself and wrote every commit on it.
    fn from_bot(&self, slug: &str) -> bool {
        let same_repo = self.head_repo.is_some() && self.head_repo == self.repo;
        self.author.as_deref() == Some(&format!("{slug}[bot]"))
            && same_repo
            && self
                .head_ref
                .as_deref()
                .is_some_and(|r| r.starts_with(BRANCH_PREFIX))
            && self.verified_commits
    }
}

/// The paths a landing may change, relative to the project root.
#[derive(Debug, Default, PartialEq)]
pub struct Allowed {
    pub files: BTreeSet<String>,
    /// Directories whose whole content is generated.
    pub dirs: Vec<String>,
    pub globs: Vec<String>,
}

impl Allowed {
    fn allows(&self, path: &str) -> bool {
        self.files.contains(path)
            || self.dirs.iter().any(|d| {
                path.strip_prefix(d.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
            })
            || self.globs.iter().any(|g| crate::glob::matches(g, path))
    }

    /// What regeneration can write here, plus the `[bot] landing_paths`
    /// globs. `changelog` is whether the repository drafts a changelog.
    fn discover(root: &Path, globs: Vec<String>, changelog: bool) -> Result<Allowed, String> {
        let mut allowed = Allowed {
            globs,
            ..Allowed::default()
        };
        if changelog {
            allowed.files.insert("CHANGELOG.md".to_string());
        }
        if !root.join("Cargo.toml").is_file() {
            return Ok(allowed);
        }
        for (_, dir) in workspace::member_dirs(Some(root)).map_err(|e| e.to_string())? {
            let Ok(rel) = dir.strip_prefix(root) else {
                continue;
            };
            let specs = spec_config::load(&dir)?;
            let generated = specs.entries.iter().filter(|e| e.mode == Mode::Generate);
            allowed
                .files
                .extend(generated.filter_map(|e| joined(rel, &e.path)));
            let sdks = sdk_config::load(&dir)?;
            allowed
                .dirs
                .extend(sdks.entries.iter().filter_map(|e| joined(rel, &e.out)));
        }
        Ok(allowed)
    }
}

/// `rel/tail` with forward slashes, or none when it is the root itself or
/// leaves it.
fn joined(rel: &Path, tail: &str) -> Option<String> {
    let mut parts = Vec::new();
    let path = rel.join(tail);
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Whether `pr` is a landing by the bot `bot` names: its author, branch and
/// commits are the bot's, and `changed` is not empty, holds no
/// `soothfast.toml` and stays inside `allowed`.
pub fn is_landing(
    pr: &PullRequest,
    bot: &Landing,
    allowed: &Allowed,
    changed: &BTreeSet<String>,
) -> bool {
    pr.from_bot(&bot.slug)
        && !changed.is_empty()
        && changed.iter().all(|path| {
            // The config read here is the pull request's own, so a landing
            // may not change one.
            let config = path.rsplit('/').next() == Some(CONFIG_NAME);
            !config && allowed.allows(path)
        })
}

/// Whether the pull request planned at `root` against `base` is a landing.
pub fn detect(root: &Path, base: Option<&str>, pr: &PullRequest, changelog: bool) -> bool {
    let Some(base) = base else {
        return false;
    };
    read(root, base, pr, changelog).unwrap_or_else(|e| {
        eprintln!("plan: cannot tell whether this is a bot landing ({e}); planning every lane");
        false
    })
}

fn read(root: &Path, base: &str, pr: &PullRequest, changelog: bool) -> Result<bool, String> {
    let text = match std::fs::read_to_string(root.join(CONFIG_NAME)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{CONFIG_NAME}: {e}")),
        Ok(text) => text,
    };
    let bot = Landing::parse(&text)?;
    if !pr.from_bot(&bot.slug) {
        return Ok(false);
    }
    let changed = committed(root, base)?;
    let allowed = Allowed::discover(root, bot.paths.clone(), changelog)?;
    Ok(is_landing(pr, &bot, &allowed, &changed))
}

/// The files the commits since `base`'s merge-base change, relative to
/// `root`. A change outside `root` is an error.
fn committed(root: &Path, base: &str) -> Result<BTreeSet<String>, String> {
    let git = |args: &[&str]| invoke::git_in(root, args).map_err(|e| e.to_string());
    let prefix = git(&["rev-parse", "--show-prefix"])?;
    let range = format!("{base}...HEAD");
    let names = git(&[
        "-c",
        "core.quotepath=false",
        "diff",
        "--name-only",
        "--no-renames",
        &range,
    ])?;
    names
        .lines()
        .map(|path| {
            path.strip_prefix(prefix.as_str())
                .map(String::from)
                .ok_or(format!("{path} is outside the project"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_directory_allows_only_what_is_under_it() {
        let allowed = Allowed {
            dirs: vec!["sdk/python".into()],
            ..Allowed::default()
        };
        assert!(allowed.allows("sdk/python/acme/models.py"));
        assert!(!allowed.allows("sdk/python"));
        assert!(!allowed.allows("sdk/python-extra/a.py"));
    }

    #[test]
    fn a_path_joins_under_its_package_and_never_names_the_root() {
        assert_eq!(
            joined(Path::new("api"), "./openapi.yaml").as_deref(),
            Some("api/openapi.yaml")
        );
        assert_eq!(joined(Path::new(""), "sdk").as_deref(), Some("sdk"));
        assert_eq!(joined(Path::new(""), "."), None);
        assert_eq!(joined(Path::new("api"), "../other"), None);
    }
}
