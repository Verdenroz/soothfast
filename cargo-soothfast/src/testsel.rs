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

use crate::glob;
use crate::invoke;
use crate::test_config::TestConfig;

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
    /// The `CARGO_PKG_*` variables cargo sets when it runs this package's
    /// tests.
    pub env: Vec<(String, String)>,
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
        &crate::workspace::member_metadata(None)?,
        root,
    ))
}

fn parse_packages(meta: &Value, root: &Path) -> Vec<Package> {
    let field = |p: &Value, k: &str| p[k].as_str().unwrap_or_default().to_string();
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
            let name = field(p, "name");
            let version = field(p, "version");
            let part = |i: usize| {
                version
                    .split(['.', '-', '+'])
                    .nth(i)
                    .unwrap_or("")
                    .to_string()
            };
            let authors: Vec<&str> = p["authors"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let env = vec![
                ("CARGO_PKG_NAME".into(), name.clone()),
                ("CARGO_PKG_VERSION".into(), version.clone()),
                ("CARGO_PKG_VERSION_MAJOR".into(), part(0)),
                ("CARGO_PKG_VERSION_MINOR".into(), part(1)),
                ("CARGO_PKG_VERSION_PATCH".into(), part(2)),
                ("CARGO_PKG_AUTHORS".into(), authors.join(":")),
                ("CARGO_PKG_DESCRIPTION".into(), field(p, "description")),
                ("CARGO_PKG_HOMEPAGE".into(), field(p, "homepage")),
                ("CARGO_PKG_REPOSITORY".into(), field(p, "repository")),
                ("CARGO_PKG_LICENSE".into(), field(p, "license")),
                ("CARGO_PKG_RUST_VERSION".into(), field(p, "rust_version")),
                (
                    "CARGO_MANIFEST_DIR".into(),
                    manifest_dir.display().to_string(),
                ),
            ];
            let deps = p["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|d| d["name"].as_str().map(String::from))
                .collect();
            Some(Package {
                name,
                dir,
                manifest_dir,
                deps,
                env,
            })
        })
        .collect()
}

/// Select the packages `changed` (root-relative paths) can affect.
pub fn select(pkgs: &[Package], changed: &BTreeSet<String>, cfg: &TestConfig) -> Selection {
    let mut direct = BTreeSet::new();
    for file in changed {
        if cfg.inputs.iter().any(|g| glob::matches(g, file)) {
            return everything(pkgs, file);
        }
        if cfg.ignore.iter().any(|g| glob::matches(g, file)) {
            continue;
        }
        if GLOBAL_FILES.contains(&file.as_str()) {
            return everything(pkgs, file);
        }
        match owner(pkgs, file) {
            Some(p) => {
                direct.insert(p.name.as_str());
            }
            None => return everything(pkgs, file),
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

/// Root-relative paths changed since `base`'s merge-base with HEAD, plus
/// uncommitted and untracked ones.
pub fn changed_files(base: &str) -> io::Result<BTreeSet<String>> {
    let range = format!("{base}...HEAD");
    let mut files = BTreeSet::new();
    for args in [
        vec!["diff", "--name-only", "--no-renames", range.as_str()],
        vec!["diff", "--name-only", "--no-renames", "HEAD"],
        vec!["ls-files", "--others", "--exclude-standard"],
    ] {
        let mut full = vec!["-c", "core.quotepath=false"];
        full.extend(args);
        files.extend(invoke::git(&full)?.lines().map(String::from));
    }
    Ok(files)
}

/// The default branch to diff against: `origin/HEAD`, else the first of
/// `origin/master`, `origin/main`, `master`, `main` that exists.
pub fn default_base() -> io::Result<String> {
    if let Ok(head) = invoke::git(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        return Ok(head);
    }
    ["origin/master", "origin/main", "master", "main"]
        .into_iter()
        .find(|b| invoke::git(&["rev-parse", "--verify", "--quiet", b]).is_ok())
        .map(String::from)
        .ok_or_else(|| io::Error::other("no default branch found; pass --base REF"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pkg(name: &str, dir: &str, deps: &[&str]) -> Package {
        Package {
            name: name.into(),
            dir: dir.into(),
            manifest_dir: PathBuf::from("/ws").join(dir),
            deps: deps.iter().map(|d| d.to_string()).collect(),
            env: Vec::new(),
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
    fn metadata_becomes_packages_with_relative_dirs_and_env() {
        let meta = json!({ "packages": [{
            "name": "core", "version": "1.2.3-rc.1", "authors": ["a", "b"],
            "manifest_path": "/ws/core/Cargo.toml",
            "dependencies": [{ "name": "serde" }, { "name": "macros" }]
        }]});
        let pkgs = parse_packages(&meta, Path::new("/ws"));
        assert_eq!(pkgs[0].dir, "core");
        assert_eq!(pkgs[0].deps, ["serde", "macros"]);
        let env: BTreeMap<_, _> = pkgs[0].env.iter().cloned().collect();
        assert_eq!(env["CARGO_PKG_VERSION_MINOR"], "2");
        assert_eq!(env["CARGO_PKG_AUTHORS"], "a:b");
        assert_eq!(env["CARGO_MANIFEST_DIR"], "/ws/core");
    }
}
