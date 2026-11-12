//! The workspace init scaffolds for: the settings saved in `soothfast.toml`,
//! what detection finds and what the flags add.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::Pins;
use super::ci::Ci;
use super::detect;
use super::ecosystem::{Ecosystem, Entry, Project, Settings, Suite, parse_projects, parse_suites};
use super::lookup::{action_ref, default_branch, pinned};
use super::rust::{benched, docs_packages, has_markers, rust_meta};
use super::workflow::{WORKFLOW_PATH, Workspace};
use crate::invoke;

/// What the scaffold under `root` derives from, and a line per project
/// found.
#[allow(clippy::too_many_arguments)]
pub(super) fn target(
    root: &Path,
    flags: Settings,
    dir: Option<String>,
    ci: Ci,
    tools: Pins,
    bench: &[String],
    check: bool,
    bot: bool,
    offline: bool,
) -> Result<(Workspace, Vec<String>), String> {
    let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap_or_default();
    let stored = parse_projects(&text).map_err(|e| format!("soothfast.toml: {e}"))?;
    let ci = ci.over(Ci::parse(&text).map_err(|e| format!("soothfast.toml: {e}"))?);
    let root_is_rust = flags.ecosystem == Some(Ecosystem::Rust);
    if root_is_rust && dir.is_some() {
        return Err("--ecosystem rust is the cargo workspace at the root; drop --dir".into());
    }
    let flags = if root_is_rust {
        Settings::default()
    } else {
        flags
    };
    let rust = root_is_rust || root.join("Cargo.toml").is_file();
    let (projects, notes) = plan(root, stored, flags, dir)?;
    if !rust && projects.is_empty() {
        return Err(if notes.is_empty() {
            "no Cargo.toml, build.gradle(.kts), pom.xml, pyproject.toml, setup.py or \
             package.json found; pass --ecosystem"
                .into()
        } else {
            notes.join("\n")
        });
    }
    let markers = has_markers(root);
    let (benched, docs_packages) = if rust {
        invoke::workspace_root().map_err(|e| format!("not in a cargo workspace: {e}"))?;
        let meta = rust_meta(root, bench, check)?;
        let docs = if markers {
            docs_packages(root, &meta, &ci)
        } else {
            Vec::new()
        };
        (benched(&meta), docs)
    } else if bench.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        return Err("-p adds a bench target to a cargo package".into());
    };
    let existing = std::fs::read_to_string(root.join(WORKFLOW_PATH)).unwrap_or_default();
    let ws = Workspace {
        rust,
        projects,
        ci,
        default_branch: default_branch(root, &existing, offline),
        benched,
        docs_packages,
        action: action_ref(&existing, tools.action.as_deref(), offline),
        bot,
        labels: false,
        pages: crate::config::Config::load(root)?.pages()?,
        java: tools
            .java
            .or_else(|| pinned(&existing, "java"))
            .unwrap_or("21".into()),
        node: tools
            .node
            .or_else(|| pinned(&existing, "node"))
            .unwrap_or("22".into()),
    };
    Ok((ws, notes))
}

/// The non-Rust projects to gate, and a line per project found: saved
/// settings over what the tracked files show, and the flags over both. A
/// project found only by detection is skipped when it has no benchmark
/// command.
fn plan(
    root: &Path,
    stored: Vec<Entry>,
    flags: Settings,
    dir: Option<String>,
) -> Result<(Vec<Project>, Vec<String>), String> {
    let detected = detect::projects(root);
    let idle: BTreeSet<String> = detected
        .iter()
        .filter(|f| !f.harness)
        .map(|f| f.dir.clone())
        .collect();
    let mut settings: BTreeMap<String, Settings> = detected
        .into_iter()
        .map(|f| {
            let found = Settings {
                ecosystem: Some(f.ecosystem),
                ..Settings::default()
            };
            (f.dir, found)
        })
        .collect();
    let mut chosen: BTreeSet<String> = BTreeSet::new();
    for Entry {
        dir,
        settings: saved,
    } in stored
    {
        let found = settings.remove(&dir).unwrap_or_default();
        settings.insert(dir.clone(), saved.over(found));
        chosen.insert(dir);
    }
    if flags != Settings::default() || dir.is_some() {
        let dir = dir.unwrap_or_else(|| ".".into());
        let current = settings.remove(&dir).unwrap_or_default();
        settings.insert(dir.clone(), flags.over(current));
        chosen.insert(dir);
    }
    let mut projects = Vec::new();
    let mut notes = Vec::new();
    for (dir, found) in settings {
        let Some(ecosystem) = found.ecosystem else {
            return Err(format!("no project detected in {dir}; pass --ecosystem"));
        };
        if idle.contains(&dir) && !chosen.contains(&dir) {
            notes.push(format!(
                "skipped  {dir} ({}): no benchmark harness found; add --dir {dir} \
                 --bench-cmd CMD --bench-out PATH",
                ecosystem.name()
            ));
            continue;
        }
        match found.bench_for(ecosystem) {
            Ok(bench) => {
                notes.push(format!("project  {dir} ({})", ecosystem.name()));
                projects.push(Project {
                    ecosystem,
                    dir,
                    bench,
                    test: found.tests(),
                    walltime: found.walltime.or(Some("hard".into())),
                    prefix: found.prefix,
                });
            }
            Err(e) if chosen.contains(&dir) => return Err(format!("{dir}: {e}")),
            Err(_) => notes.push(format!(
                "skipped  {dir} ({}): no benchmark command; add --dir {dir} \
                 --bench-cmd CMD --bench-out PATH",
                ecosystem.name()
            )),
        }
    }
    Ok((projects, notes))
}

/// The runner labels and `[test] ci` setting of the repo at `root`.
pub fn ci(root: &Path) -> Result<Ci, String> {
    let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap_or_default();
    Ci::parse(&text).map_err(|e| format!("soothfast.toml: {e}"))
}

/// The non-Rust projects `soothfast.toml` saves, with their benchmark
/// settings resolved.
pub fn manifest(root: &Path) -> Result<Vec<Project>, String> {
    let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap_or_default();
    let entries = parse_projects(&text).map_err(|e| format!("soothfast.toml: {e}"))?;
    entries
        .into_iter()
        .map(|Entry { dir, settings }| {
            let ecosystem = settings
                .ecosystem
                .ok_or_else(|| format!("{dir}: a project needs an ecosystem"))?;
            let bench = settings
                .bench_for(ecosystem)
                .map_err(|e| format!("{dir}: {e}"))?;
            Ok(Project {
                ecosystem,
                dir,
                bench,
                test: settings.tests(),
                walltime: settings.walltime.or(Some("hard".into())),
                prefix: settings.prefix,
            })
        })
        .collect()
}

/// The `[[test.suite]]` tables saved in `soothfast.toml`.
pub fn suites(root: &Path) -> Result<Vec<Suite>, String> {
    let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap_or_default();
    parse_suites(&text).map_err(|e| format!("soothfast.toml: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::test_support::scratch;
    #[test]
    fn a_saved_project_is_kept_over_detection_and_flags_win() {
        let root = scratch("plan");
        let git = |args: &[&str]| invoke::git_in(&root, args).unwrap();
        git(&["init", "-q"]);
        std::fs::create_dir_all(root.join("client")).unwrap();
        std::fs::write(root.join("client/settings.gradle.kts"), "").unwrap();
        let bench = r#"{"scripts":{"bench":"vitest bench"},"devDependencies":{"vitest":"1"}}"#;
        std::fs::write(root.join("package.json"), bench).unwrap();
        std::fs::create_dir_all(root.join("tools")).unwrap();
        std::fs::write(
            root.join("tools/pyproject.toml"),
            "[project]\nname = \"tools\"\n",
        )
        .unwrap();
        git(&["add", "-A"]);
        let (projects, notes) = plan(&root, Vec::new(), Settings::default(), None).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].dir, ".");
        assert_eq!(projects[0].walltime.as_deref(), Some("hard"));
        assert!(notes.iter().any(|n| {
            n.starts_with("skipped  tools (python): no benchmark harness found; add --dir tools")
        }));
        assert!(
            notes
                .iter()
                .any(|n| n.starts_with("skipped  client (gradle)"))
        );
        assert!(
            notes
                .iter()
                .any(|n| n.contains("--dir client --bench-cmd CMD"))
        );

        let flags = Settings {
            bench_cmd: Some("./gradlew jmh -Pout={out}".into()),
            ..Settings::default()
        };
        let (projects, _) = plan(&root, Vec::new(), flags, Some("client".into())).unwrap();
        let dirs: Vec<_> = projects.iter().map(|p| p.dir.as_str()).collect();
        assert_eq!(dirs, [".", "client"]);
        assert_eq!(
            projects[1].bench.as_ref().unwrap().cmd,
            "./gradlew jmh -Pout={out}"
        );

        let by_flag = Settings {
            ecosystem: Some(Ecosystem::Python),
            ..Settings::default()
        };
        let (projects, _) = plan(&root, Vec::new(), by_flag, Some("plugin/hooks".into())).unwrap();
        assert!(
            projects
                .iter()
                .any(|p| p.dir == "plugin/hooks" && p.ecosystem == Ecosystem::Python)
        );

        let none = plan(
            &root,
            Vec::new(),
            Settings::default(),
            Some("nowhere".into()),
        );
        assert!(none.unwrap_err().contains("nowhere"));
    }
}
