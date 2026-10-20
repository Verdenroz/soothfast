//! Which workspace packages a change can affect.
//!
//! A changed file belongs to the package whose manifest dir holds it. That
//! package and every workspace package that depends on it, through any
//! dependency kind, is selected. Files that change how everything builds,
//! or that no package owns, select every package.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::config::TestConfig;
use crate::{glob, invoke};

/// Files whose change can alter every package's build.
const GLOBAL_FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain",
    "rust-toolchain.toml",
    ".cargo/config",
    ".cargo/config.toml",
];

/// One workspace package, as far as test selection and running need it.
#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub name: String,
    /// Manifest dir relative to the workspace root, `/`-separated; empty for
    /// a root package.
    pub dir: String,
    pub manifest_dir: PathBuf,
    /// Names of the packages this one declares a dependency on, any kind.
    pub deps: Vec<String>,
}

/// The packages selected, and why every one was if all were.
#[derive(Debug, PartialEq)]
pub struct Selection {
    pub names: BTreeSet<String>,
    pub everything_because: Option<String>,
}

/// The workspace's packages, from `cargo metadata`.
pub fn packages(root: &Path) -> io::Result<Vec<Package>> {
    Ok(parse_packages(
        &crate::workspace::metadata(None, true)?,
        root,
    ))
}

fn parse_packages(meta: &Value, root: &Path) -> Vec<Package> {
    meta["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let manifest = PathBuf::from(p["manifest_path"].as_str()?);
            let manifest_dir = manifest.parent()?.to_path_buf();
            let dir = manifest_dir
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            let deps = p["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|d| d["name"].as_str().map(String::from))
                .collect();
            Some(Package {
                name: p["name"].as_str()?.to_string(),
                dir,
                manifest_dir,
                deps,
            })
        })
        .collect()
}

fn matches_any(globs: &[String], file: &str) -> bool {
    globs.iter().any(|g| glob::matches(g, file))
}

/// What a change to one file reaches.
#[derive(Debug, PartialEq)]
pub enum Scope<'a> {
    /// Nothing: `ignore` covers it.
    Ignored,
    /// The named package and its dependents.
    Package(&'a str),
    /// Every package.
    Workspace,
}

/// What a change to `file` reaches. A declared input wins over `ignore`,
/// and `ignore` wins over a build-wide or unowned file.
pub fn classify<'a>(pkgs: &'a [Package], cfg: &TestConfig, file: &str) -> Scope<'a> {
    if matches_any(&cfg.inputs, file) {
        return Scope::Workspace;
    }
    if matches_any(&cfg.ignore, file) {
        return Scope::Ignored;
    }
    if GLOBAL_FILES.contains(&file) {
        return Scope::Workspace;
    }
    owner(pkgs, file).map_or(Scope::Workspace, |p| Scope::Package(&p.name))
}

/// Select the packages `changed` (root-relative paths) can affect.
pub fn select(pkgs: &[Package], changed: &BTreeSet<String>, cfg: &TestConfig) -> Selection {
    let mut direct = BTreeSet::new();
    for file in changed {
        match classify(pkgs, cfg, file) {
            Scope::Workspace => return everything(pkgs, file),
            Scope::Ignored => {}
            Scope::Package(name) => {
                direct.insert(name);
            }
        }
    }
    Selection {
        names: with_dependents(pkgs, direct),
        everything_because: None,
    }
}

/// Every package, with the reason.
pub fn everything(pkgs: &[Package], because: &str) -> Selection {
    Selection {
        names: pkgs.iter().map(|p| p.name.clone()).collect(),
        everything_because: Some(because.to_string()),
    }
}

/// The package with the longest manifest dir holding `file`.
fn owner<'a>(pkgs: &'a [Package], file: &str) -> Option<&'a Package> {
    pkgs.iter()
        .filter(|p| {
            p.dir.is_empty()
                || file
                    .strip_prefix(p.dir.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|p| p.dir.len())
}

fn with_dependents(pkgs: &[Package], direct: BTreeSet<&str>) -> BTreeSet<String> {
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for p in pkgs {
        for d in &p.deps {
            dependents.entry(d.as_str()).or_default().push(&p.name);
        }
    }
    let mut seen: BTreeSet<&str> = direct.clone();
    let mut queue: Vec<&str> = direct.into_iter().collect();
    while let Some(name) = queue.pop() {
        for &dep in dependents.get(name).into_iter().flatten() {
            if seen.insert(dep) {
                queue.push(dep);
            }
        }
    }
    seen.into_iter().map(String::from).collect()
}

/// Workspace-relative paths changed since `base`'s merge-base with HEAD,
/// plus uncommitted and untracked ones. The workspace may sit below the git
/// toplevel; `--relative` and `ls-files` both report paths from `root`.
pub fn changed_files(root: &Path, base: &str) -> io::Result<BTreeSet<String>> {
    let range = format!("{base}...HEAD");
    let mut files = BTreeSet::new();
    for args in [
        vec!["diff", "--name-only", "--no-renames", "--relative", &range],
        vec!["diff", "--name-only", "--no-renames", "--relative", "HEAD"],
        vec!["ls-files", "--others", "--exclude-standard"],
    ] {
        let mut full = vec!["-c", "core.quotepath=false"];
        full.extend(args);
        files.extend(invoke::git_in(root, &full)?.lines().map(String::from));
    }
    Ok(files)
}

/// Whether `base` and HEAD share history; an unknown `base` is an error.
pub fn has_merge_base(root: &Path, base: &str) -> io::Result<bool> {
    let commit = format!("{base}^{{commit}}");
    invoke::git_in(root, &["rev-parse", "--verify", "--quiet", &commit])
        .map_err(|_| io::Error::other(format!("unknown ref {base:?}; pass --base REF")))?;
    Ok(invoke::git_in(root, &["merge-base", base, "HEAD"]).is_ok())
}

/// The default branch to diff against: `origin/HEAD`, else the first of
/// `origin/master`, `origin/main`, `master`, `main` that exists.
pub fn default_base(root: &Path) -> io::Result<String> {
    let git = |args: &[&str]| invoke::git_in(root, args);
    if let Ok(head) = git(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        return Ok(head);
    }
    ["origin/master", "origin/main", "master", "main"]
        .into_iter()
        .find(|b| git(&["rev-parse", "--verify", "--quiet", b]).is_ok())
        .map(String::from)
        .ok_or_else(|| io::Error::other("no default branch found; pass --base REF"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::fixture::TempRepo;
    use serde_json::json;

    fn pkg(name: &str, dir: &str, deps: &[&str]) -> Package {
        Package {
            name: name.into(),
            dir: dir.into(),
            manifest_dir: PathBuf::from("/ws").join(dir),
            deps: deps.iter().map(|d| d.to_string()).collect(),
        }
    }

    fn graph() -> Vec<Package> {
        vec![
            pkg("core", "core", &[]),
            pkg("macros", "core/macros", &[]),
            pkg("server", "server", &["core"]),
            pkg("cli", "cli", &["server"]),
            pkg("unrelated", "unrelated", &[]),
        ]
    }

    fn files(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|f| f.to_string()).collect()
    }

    fn names(sel: &Selection) -> Vec<&str> {
        sel.names.iter().map(String::as_str).collect()
    }

    #[test]
    fn a_change_selects_its_package_and_transitive_dependents() {
        let sel = select(
            &graph(),
            &files(&["core/src/lib.rs"]),
            &TestConfig::default(),
        );
        assert_eq!(names(&sel), ["cli", "core", "server"]);
        assert_eq!(sel.everything_because, None);
    }

    #[test]
    fn the_longest_manifest_dir_owns_a_nested_file() {
        let sel = select(
            &graph(),
            &files(&["core/macros/src/lib.rs"]),
            &TestConfig::default(),
        );
        assert_eq!(names(&sel), ["macros"]);
    }

    #[test]
    fn a_sibling_dir_sharing_a_prefix_does_not_own_the_file() {
        let sel = select(
            &graph(),
            &files(&["server-extras/x.rs"]),
            &TestConfig::default(),
        );
        assert_eq!(
            sel.everything_because.as_deref(),
            Some("server-extras/x.rs")
        );
    }

    #[test]
    fn dev_dependents_count() {
        let pkgs = vec![
            pkg("lib", "lib", &[]),
            pkg("fixtures", "fixtures", &["lib"]),
        ];
        let sel = select(&pkgs, &files(&["lib/a.rs"]), &TestConfig::default());
        assert_eq!(names(&sel), ["fixtures", "lib"]);
    }

    #[test]
    fn a_dependency_cycle_terminates() {
        let pkgs = vec![pkg("a", "a", &["b"]), pkg("b", "b", &["a"])];
        let sel = select(&pkgs, &files(&["a/x.rs"]), &TestConfig::default());
        assert_eq!(names(&sel), ["a", "b"]);
    }

    #[test]
    fn global_files_select_everything() {
        for f in [
            "Cargo.lock",
            "Cargo.toml",
            "rust-toolchain.toml",
            ".cargo/config.toml",
        ] {
            let sel = select(&graph(), &files(&[f]), &TestConfig::default());
            assert_eq!(sel.names.len(), 5, "{f}");
            assert_eq!(sel.everything_because.as_deref(), Some(f));
        }
    }

    #[test]
    fn a_declared_input_inside_a_package_selects_everything() {
        let cfg = TestConfig {
            inputs: vec!["unrelated/shared/**".into()],
            ..TestConfig::default()
        };
        let sel = select(&graph(), &files(&["unrelated/shared/f.json"]), &cfg);
        assert_eq!(sel.names.len(), 5);
        assert_eq!(
            sel.everything_because.as_deref(),
            Some("unrelated/shared/f.json")
        );
    }

    #[test]
    fn a_declared_input_wins_over_ignore() {
        let cfg = TestConfig {
            ignore: vec!["docs/**".into()],
            inputs: vec!["docs/claims.md".into()],
            ..TestConfig::default()
        };
        let sel = select(&graph(), &files(&["docs/claims.md"]), &cfg);
        assert_eq!(sel.names.len(), 5);
    }

    #[test]
    fn a_file_outside_every_package_selects_everything() {
        let sel = select(&graph(), &files(&["README.md"]), &TestConfig::default());
        assert_eq!(sel.names.len(), 5);
    }

    #[test]
    fn an_ignored_file_selects_nothing() {
        let cfg = TestConfig {
            ignore: vec!["docs/**".into(), "*.md".into()],
            ..TestConfig::default()
        };
        let sel = select(&graph(), &files(&["docs/a.md", "README.md"]), &cfg);
        assert!(sel.names.is_empty());
        assert_eq!(sel.everything_because, None);
    }

    #[test]
    fn ignore_does_not_cover_a_package_file_it_does_not_match() {
        let cfg = TestConfig {
            ignore: vec!["docs/**".into()],
            ..TestConfig::default()
        };
        let sel = select(&graph(), &files(&["docs/a.md", "unrelated/b.rs"]), &cfg);
        assert_eq!(names(&sel), ["unrelated"]);
    }

    #[test]
    fn no_change_selects_nothing() {
        let sel = select(&graph(), &BTreeSet::new(), &TestConfig::default());
        assert!(sel.names.is_empty());
    }

    #[test]
    fn workspace_wide_covers_globals_and_unowned_files_unless_ignored() {
        let cfg = TestConfig {
            ignore: vec!["*.md".into()],
            ..TestConfig::default()
        };
        let pkgs = graph();
        let wide = |f: &str| classify(&pkgs, &cfg, f) == Scope::Workspace;
        assert!(wide(".cargo/config.toml"));
        assert!(wide("Makefile"));
        assert!(!wide("README.md"));
        assert!(!wide("core/src/lib.rs"));
    }

    #[test]
    fn metadata_becomes_packages_with_relative_dirs() {
        let meta = json!({ "packages": [{
            "name": "core",
            "manifest_path": "/ws/core/Cargo.toml",
            "dependencies": [{ "name": "serde" }, { "name": "macros" }]
        }]});
        let pkgs = parse_packages(&meta, Path::new("/ws"));
        assert_eq!(pkgs[0].dir, "core");
        assert_eq!(pkgs[0].deps, ["serde", "macros"]);
    }

    #[test]
    fn changes_are_reported_from_a_workspace_below_the_git_toplevel() {
        let repo = TempRepo::new("sel");
        repo.write("rust/pkg/a.rs", "a");
        repo.write("outside.txt", "o");
        repo.git(&["add", "."]);
        repo.commit();
        repo.write("rust/pkg/a.rs", "b");
        repo.write("rust/pkg/new.rs", "n");
        repo.write("outside.txt", "p");
        let got = changed_files(&repo.root.join("rust"), "HEAD").unwrap();
        assert_eq!(got, files(&["pkg/a.rs", "pkg/new.rs"]));
    }

    #[test]
    fn unrelated_history_has_no_merge_base_and_an_unknown_ref_is_an_error() {
        let repo = TempRepo::new("mb");
        repo.write("a", "a");
        repo.git(&["add", "."]);
        repo.commit();
        let first = repo.git(&["rev-parse", "HEAD"]);
        repo.git(&["checkout", "-q", "--orphan", "other"]);
        repo.write("a", "b");
        repo.git(&["add", "."]);
        repo.commit();
        assert!(has_merge_base(&repo.root, "HEAD").unwrap());
        assert!(!has_merge_base(&repo.root, &first).unwrap());
        assert!(has_merge_base(&repo.root, "nope").is_err());
    }
}
