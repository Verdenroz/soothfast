//! Whether a pull request is one of soothfast-bot's own landings: regenerated
//! files only, which the lanes have nothing to say about. Anything unknown
//! or unreadable makes it an ordinary pull request.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use crate::ci::regen::changelog_listed;
use crate::config::Config;
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

    /// What regeneration can write for the workspace `members` under `root`,
    /// plus the `[bot] landing_paths` globs. `changelog` is whether
    /// regeneration drafts a changelog.
    fn of(
        root: &Path,
        members: &[(String, PathBuf)],
        globs: Vec<String>,
        changelog: bool,
    ) -> Result<Allowed, String> {
        let mut allowed = Allowed {
            globs,
            ..Allowed::default()
        };
        if changelog {
            allowed.files.insert("CHANGELOG.md".to_string());
        }
        for (_, dir) in members {
            let Ok(rel) = dir.strip_prefix(root) else {
                continue;
            };
            let specs = spec_config::load(dir)?;
            let generated = specs.entries.iter().filter(|e| e.mode == Mode::Generate);
            allowed
                .files
                .extend(generated.filter_map(|e| joined(rel, &e.path)));
            let sdks = sdk_config::load(dir)?;
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
pub fn detect(root: &Path, base: Option<&str>, pr: &PullRequest) -> bool {
    let Some(base) = base else {
        return false;
    };
    read(root, base, pr).unwrap_or_else(|e| {
        eprintln!("plan: cannot tell whether this is a bot landing ({e}); planning every lane");
        false
    })
}

fn read(root: &Path, base: &str, pr: &PullRequest) -> Result<bool, String> {
    let config = Config::load(root)?;
    let bot = config.landing()?;
    if !pr.from_bot(&bot.slug) {
        return Ok(false);
    }
    let changed = committed(root, base)?;
    let (members, gated) = match root.join("Cargo.toml").is_file() {
        true => (
            workspace::member_dirs(Some(root)).map_err(|e| e.to_string())?,
            workspace::bench_crates("soothfast", Some(root))?,
        ),
        false => Default::default(),
    };
    let packages = config.changelog()?.packages;
    let changelog = !changelog_listed(&packages, &gated).is_empty();
    let allowed = Allowed::of(root, &members, bot.paths.clone(), changelog)?;
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
    use crate::test::fixture::{TempDir, TempRepo};

    fn bot_pr() -> PullRequest {
        PullRequest {
            author: Some("soothfast-bot[bot]".into()),
            head_ref: Some("bot/soothfast-update".into()),
            head_repo: Some("acme/items".into()),
            repo: Some("acme/items".into()),
            verified_commits: true,
        }
    }

    /// A repository whose `base` branch moved on after `HEAD` branched off
    /// to change `CHANGELOG.md` and `web/app.js`, with one untracked file.
    fn branched(name: &str) -> TempRepo {
        let repo = TempRepo::new(name);
        repo.write("CHANGELOG.md", "one\n");
        repo.write("web/app.js", "one\n");
        repo.write("README.md", "one\n");
        repo.git(&["add", "-A"]);
        repo.commit();
        repo.git(&["branch", "base"]);
        repo.git(&["checkout", "-q", "-b", "bot/soothfast-update"]);
        repo.write("CHANGELOG.md", "two\n");
        repo.write("web/app.js", "two\n");
        repo.git(&["add", "-A"]);
        repo.commit();
        repo.git(&["checkout", "-q", "base"]);
        repo.write("README.md", "two\n");
        repo.git(&["add", "-A"]);
        repo.commit();
        repo.git(&["checkout", "-q", "bot/soothfast-update"]);
        repo.write("stray.txt", "untracked\n");
        repo
    }

    #[test]
    fn the_committed_diff_is_what_the_branch_changed_and_nothing_else() {
        let repo = branched("landing-committed");
        let changed = committed(&repo.root, "base").unwrap();
        let expected: BTreeSet<String> = ["CHANGELOG.md", "web/app.js"].map(String::from).into();
        assert_eq!(changed, expected);
        assert!(committed(&repo.root, "no-such-ref").is_err());
    }

    #[test]
    fn a_change_outside_a_nested_project_is_an_error() {
        let repo = branched("landing-nested");
        let err = committed(&repo.root.join("web"), "base").unwrap_err();
        assert!(err.contains("CHANGELOG.md is outside the project"), "{err}");
        repo.git(&["checkout", "-q", "base"]);
        repo.git(&["checkout", "-q", "-b", "bot/web-only"]);
        repo.write("web/app.js", "three\n");
        repo.git(&["add", "web"]);
        repo.commit();
        let changed = committed(&repo.root.join("web"), "base").unwrap();
        assert_eq!(changed, ["app.js".to_string()].into());
    }

    #[test]
    fn the_allowed_set_is_what_each_member_generates() {
        let root = TempDir::new("landing-allowed");
        let api = root.join("crates/api");
        std::fs::create_dir_all(&api).unwrap();
        let config = "[[spec]]\npath = \"openapi.yaml\"\nmode = \"generate\"\n\n\
            [[spec]]\npath = \"vendor/upstream.yaml\"\n\n\
            [[sdk]]\nspec = \"openapi.yaml\"\nlang = \"python\"\nout = \"sdk/python\"\npackage = \"acme-items\"\n";
        std::fs::write(api.join("soothfast.toml"), config).unwrap();
        let members = [
            ("api".to_string(), api),
            ("core".to_string(), root.join("crates/core")),
            ("elsewhere".to_string(), PathBuf::from("/nowhere/else")),
        ];
        let allowed = Allowed::of(&root, &members, vec!["TREND.md".into()], true).unwrap();
        let expected = Allowed {
            files: ["CHANGELOG.md", "crates/api/openapi.yaml"]
                .map(String::from)
                .into(),
            dirs: vec!["crates/api/sdk/python".into()],
            globs: vec!["TREND.md".into()],
        };
        assert_eq!(allowed, expected);
        let bare = Allowed::of(&root, &members[1..], Vec::new(), false).unwrap();
        assert_eq!(bare, Allowed::default());
    }

    #[test]
    fn a_malformed_table_only_matters_once_the_bot_opened_the_pull_request() {
        let repo = branched("landing-malformed");
        repo.write("soothfast.toml", "[changelog]\npackages = \"core\"\n");
        let human = PullRequest {
            author: Some("octocat".into()),
            ..bot_pr()
        };
        assert_eq!(read(&repo.root, "base", &human), Ok(false));
        assert!(read(&repo.root, "base", &bot_pr()).is_err());
        assert!(!detect(&repo.root, Some("base"), &bot_pr()));
        assert!(!detect(&repo.root, None, &bot_pr()));
    }

    #[test]
    fn a_landing_is_detected_from_the_repository_itself() {
        let repo = branched("landing-detect");
        assert!(!detect(&repo.root, Some("base"), &bot_pr()));
        repo.git(&["checkout", "-q", "base"]);
        repo.write(
            "soothfast.toml",
            "[bot]\nlanding_paths = [\"CHANGELOG.md\", \"web/**\"]\n",
        );
        repo.git(&["add", "soothfast.toml"]);
        repo.commit();
        repo.git(&["checkout", "-q", "bot/soothfast-update"]);
        repo.git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "merge",
            "-q",
            "base",
        ]);
        assert!(detect(&repo.root, Some("base"), &bot_pr()));
    }

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
