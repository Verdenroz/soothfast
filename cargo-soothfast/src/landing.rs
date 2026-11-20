//! Whether a pull request is one of soothfast-bot's own landings: regenerated
//! files only, which the lanes have nothing to say about. Anything unknown
//! or unreadable makes it an ordinary pull request.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use crate::ci::capture;
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
    /// Name of the head branch.
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
    fn opened_by_bot(&self, slug: &str) -> bool {
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
    /// Single generated files.
    pub files: BTreeSet<String>,
    /// Directories whose whole content is generated.
    pub dirs: Vec<String>,
    /// The `[bot] landing_paths` globs.
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
    pr.opened_by_bot(&bot.slug) && only_generated(allowed, changed, &BTreeSet::new())
}

/// Whether `changed` is not empty, holds no `soothfast.toml`, and every
/// path in it is inside `allowed` or among `also`.
fn only_generated(allowed: &Allowed, changed: &BTreeSet<String>, also: &BTreeSet<String>) -> bool {
    !changed.is_empty()
        && changed.iter().all(|path| {
            // The config read here is the change's own, so generated files
            // may not come with one.
            let config = path.rsplit('/').next() == Some(CONFIG_NAME);
            !config && (allowed.allows(path) || also.contains(path))
        })
}

/// Whether the commits since `base` are the bot's and changed only files
/// regeneration writes: the generated paths, and whatever they changed
/// under the `hook` pathspecs. Regenerating after such a push would answer
/// its own landing. Anything unreadable is an ordinary push.
pub fn generated_push(root: &Path, base: &str, hook: &[String]) -> bool {
    let read = || -> Result<bool, String> {
        let config = Config::load(root)?;
        let bot = format!("{}[bot]", config.landing()?.slug);
        let range = format!("{base}..HEAD");
        let authors =
            invoke::git_in(root, &["log", "--format=%an", &range]).map_err(|e| e.to_string())?;
        if authors.lines().any(|author| author != bot) {
            return Ok(false);
        }
        let allowed = allowed(root, &config)?;
        let changed = committed(root, base, &[])?;
        let hooked = match hook.is_empty() {
            true => BTreeSet::new(),
            false => committed(root, base, hook)?,
        };
        Ok(only_generated(&allowed, &changed, &hooked))
    };
    read().unwrap_or_else(|e| {
        eprintln!(
            "regen: cannot tell whether this push only changed generated files ({e}); regenerating"
        );
        false
    })
}

/// The `generated=` and `bot=` lines `ci regen --probe` prints for a push
/// from `base` to `HEAD`: whether it only landed generated files, and the
/// bot's App slug, empty when the configuration cannot be read.
pub fn probe(root: &Path, base: Option<&str>, hook: &[String]) -> String {
    let generated = base.is_some_and(|base| generated_push(root, base, hook));
    let slug = Config::load(root)
        .and_then(|config| config.landing())
        .map(|landing| landing.slug)
        .unwrap_or_default();
    format!("generated={generated}\nbot={slug}\n")
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
    if !pr.opened_by_bot(&bot.slug) {
        return Ok(false);
    }
    let changed = committed(root, base, &[])?;
    let allowed = allowed(root, &config)?;
    Ok(is_landing(pr, &bot, &allowed, &changed))
}

/// What regeneration can write under `root`, plus `[bot] landing_paths`.
fn allowed(root: &Path, config: &Config) -> Result<Allowed, String> {
    let (members, gated, captured) = match root.join("Cargo.toml").is_file() {
        true => (
            workspace::member_dirs(Some(root)).map_err(|e| e.to_string())?,
            workspace::bench_crates("soothfast", Some(root))?,
            capture::regenerated(root, config)?,
        ),
        false => Default::default(),
    };
    let packages = config.changelog()?.packages;
    let changelog = !changelog_listed(&packages, &gated).is_empty();
    let mut allowed = Allowed::of(root, &members, config.landing()?.paths, changelog)?;
    allowed
        .files
        .extend(captured.into_iter().flat_map(|c| c.pages));
    Ok(allowed)
}

/// The files the commits since `base`'s merge-base change, relative to
/// `root`, limited to `pathspecs` when there are any. A change outside
/// `root` is an error.
fn committed(root: &Path, base: &str, pathspecs: &[String]) -> Result<BTreeSet<String>, String> {
    let git = |args: &[&str]| invoke::git_in(root, args).map_err(|e| e.to_string());
    let prefix = git(&["rev-parse", "--show-prefix"])?;
    let range = format!("{base}...HEAD");
    let mut args = vec![
        "-c",
        "core.quotepath=false",
        "diff",
        "--name-only",
        "--no-renames",
        &range,
        "--",
    ];
    args.extend(pathspecs.iter().map(String::as_str));
    let names = git(&args)?;
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
        let changed = committed(&repo.root, "base", &[]).unwrap();
        let expected: BTreeSet<String> = ["CHANGELOG.md", "web/app.js"].map(String::from).into();
        assert_eq!(changed, expected);
        assert!(committed(&repo.root, "no-such-ref", &[]).is_err());
    }

    #[test]
    fn a_change_outside_a_nested_project_is_an_error() {
        let repo = branched("landing-nested");
        let err = committed(&repo.root.join("web"), "base", &[]).unwrap_err();
        assert!(err.contains("CHANGELOG.md is outside the project"), "{err}");
        repo.git(&["checkout", "-q", "base"]);
        repo.git(&["checkout", "-q", "-b", "bot/web-only"]);
        repo.write("web/app.js", "three\n");
        repo.git(&["add", "web"]);
        repo.commit();
        let changed = committed(&repo.root.join("web"), "base", &[]).unwrap();
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

    /// A repository that drafts no changelog of its own, whose last commit
    /// changed `files`. `HEAD^` is the push's base.
    fn pushed(name: &str, files: &[&str]) -> TempRepo {
        let repo = TempRepo::new(name);
        repo.write(
            "soothfast.toml",
            "[bot]\nlanding_paths = [\"CHANGELOG.md\"]\n",
        );
        repo.write("CHANGELOG.md", "one\n");
        repo.write("docs/guide.md", "one\n");
        repo.write("src/lib.rs", "one\n");
        repo.git(&["add", "-A"]);
        repo.commit();
        for file in files {
            repo.write(file, "two\n");
        }
        repo.git(&["add", "-A"]);
        repo.git(&[
            "-c",
            "user.name=soothfast-bot[bot]",
            "-c",
            "user.email=bot@example.com",
            "commit",
            "-q",
            "-m",
            "land",
        ]);
        repo
    }

    #[test]
    fn a_bot_push_of_generated_files_alone_is_a_generated_push() {
        let repo = pushed("landing-push-generated", &["CHANGELOG.md"]);
        assert!(generated_push(&repo.root, "HEAD^", &[]));
    }

    #[test]
    fn the_same_files_pushed_by_anyone_else_are_not() {
        let repo = pushed("landing-push-human", &["CHANGELOG.md"]);
        repo.write("CHANGELOG.md", "three\n");
        repo.git(&["add", "-A"]);
        repo.commit();
        assert!(!generated_push(&repo.root, "HEAD^", &[]));
    }

    #[test]
    fn a_push_that_changes_anything_else_is_not() {
        let repo = pushed("landing-push-mixed", &["CHANGELOG.md", "src/lib.rs"]);
        assert!(!generated_push(&repo.root, "HEAD^", &[]));
        let config = pushed("landing-push-config", &["CHANGELOG.md", "soothfast.toml"]);
        assert!(!generated_push(&config.root, "HEAD^", &[]));
    }

    #[test]
    fn what_the_hook_may_change_counts_as_generated() {
        let repo = pushed("landing-push-hook", &["CHANGELOG.md", "docs/guide.md"]);
        assert!(!generated_push(&repo.root, "HEAD^", &[]));
        assert!(generated_push(&repo.root, "HEAD^", &["docs".to_string()]));
        assert!(!generated_push(&repo.root, "HEAD^", &["src".to_string()]));
    }

    /// A one-package repository whose README holds a capture block, and
    /// whose last commit, by `author`, changed `files`. `HEAD^` is the base.
    fn captured(name: &str, config: &str, author: &str, files: &[&str]) -> TempRepo {
        let repo = TempRepo::new(name);
        repo.write("soothfast.toml", config);
        repo.write(
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        repo.write("src/lib.rs", "one\n");
        repo.write(
            "README.md",
            "<!-- soothfast:bind demo::f -->\n```rust capture-output\nfn main() {}\n```\n",
        );
        repo.write("docs/guide.md", "one\n");
        repo.git(&["add", "-A"]);
        repo.commit();
        for file in files {
            let text = std::fs::read_to_string(repo.root.join(file)).unwrap();
            repo.write(file, &format!("{text}two\n"));
        }
        repo.git(&["add", "-A"]);
        repo.git(&[
            "-c",
            &format!("user.name={author}"),
            "-c",
            "user.email=bot@example.com",
            "commit",
            "-q",
            "-m",
            "land",
        ]);
        repo
    }

    #[test]
    fn captured_docs_are_generated_with_no_landing_path_for_them() {
        let bot = "soothfast-bot[bot]";
        let repo = captured("landing-captured", "", bot, &["README.md"]);

        assert!(generated_push(&repo.root, "HEAD^", &[]));
        let mixed = captured(
            "landing-captured-mixed",
            "",
            bot,
            &["README.md", "src/lib.rs"],
        );
        assert!(!generated_push(&mixed.root, "HEAD^", &[]));
        let prose = captured("landing-captured-prose", "", bot, &["docs/guide.md"]);
        assert!(!generated_push(&prose.root, "HEAD^", &[]));
    }

    #[test]
    fn a_human_edit_to_a_captured_doc_is_not_generated() {
        let repo = captured("landing-captured-human", "", "octocat", &["README.md"]);
        assert!(!generated_push(&repo.root, "HEAD^", &[]));
    }

    #[test]
    fn skipping_capture_leaves_its_docs_out_of_what_a_landing_may_change() {
        let config = "[ci]\nskip = [\"capture\"]\n";
        let repo = captured(
            "landing-captured-skipped",
            config,
            "soothfast-bot[bot]",
            &["README.md"],
        );
        assert!(!generated_push(&repo.root, "HEAD^", &[]));
    }

    #[test]
    fn an_empty_or_unreadable_push_is_not_generated() {
        let repo = pushed("landing-push-empty", &["CHANGELOG.md"]);
        assert!(!generated_push(&repo.root, "HEAD", &[]));
        assert!(!generated_push(&repo.root, "no-such-ref", &[]));
    }

    #[test]
    fn the_probe_answers_a_bot_landing_a_human_push_and_an_unreadable_range() {
        let landing = pushed("landing-probe-bot", &["CHANGELOG.md"]);
        let answer = probe(&landing.root, Some("HEAD^"), &[]);
        assert_eq!(answer, "generated=true\nbot=soothfast-bot\n");
        let human = pushed("landing-probe-human", &["src/lib.rs"]);
        let answer = probe(&human.root, Some("HEAD^"), &[]);
        assert_eq!(answer, "generated=false\nbot=soothfast-bot\n");
        let answer = probe(&landing.root, Some("no-such-ref"), &[]);
        assert_eq!(answer, "generated=false\nbot=soothfast-bot\n");
        assert_eq!(probe(&landing.root, None, &[]), answer);
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
