//! The public surface of non-Rust projects as `report` consumes it: listed
//! in `llms.txt`, inventoried or diffed in the changelog.

use std::path::Path;

use soothfast_docs::diff;
use soothfast_docs::surface::{ItemInfo, Surface};
use soothfast_report::llms::{ProjectEntry, ProjectSurface, SurfaceEntry};

use crate::coverage_lang::{self, Entry, Target};
use crate::invoke;

/// The project's public symbols on disk, ids relative to the repository root.
pub fn current(target: &Target) -> Result<Vec<Entry>, String> {
    let mut entries = coverage_lang::entries(target)?;
    for e in &mut entries {
        e.id = format!("{}/{}", target.label, e.id);
    }
    Ok(entries)
}

/// The project's public symbols as committed at `refname`. A project that
/// did not exist there has none.
pub fn at_ref(root: &Path, target: &Target, refname: &str) -> Result<Vec<Entry>, String> {
    let listing = invoke::git_in(
        root,
        &["ls-tree", "-r", "--name-only", refname, "--", &target.label],
    )
    .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for path in listing.lines() {
        let rel = Path::new(path)
            .strip_prefix(&target.label)
            .unwrap_or(Path::new(path));
        let Some(lang) = coverage_lang::covered(rel, &target.langs) else {
            continue;
        };
        let source = invoke::git_in(root, &["show", &format!("{refname}:{path}")])
            .map_err(|e| e.to_string())?;
        out.extend(coverage_lang::file_entries(path, lang, &source)?);
    }
    Ok(out)
}

/// `ADDED`/`REMOVED` lines for the project between `refname` and now, empty
/// when its public symbols did not change.
pub fn diff_text(root: &Path, target: &Target, refname: &str) -> Result<String, String> {
    let old = surface(&at_ref(root, target, refname)?);
    let new = surface(&current(target)?);
    let delta = diff::compare(&old, &new);
    Ok(if delta.is_empty() {
        String::new()
    } else {
        diff::render(&delta)
    })
}

fn surface(entries: &[Entry]) -> Surface {
    let mut surface = Surface::default();
    for e in entries {
        surface.items.insert(
            e.id.clone(),
            ItemInfo {
                kind: e.kind.clone(),
                fingerprint: 0,
                signature: String::new(),
                has_docs: e.documented,
            },
        );
    }
    surface
}

/// The project's symbols as changelog inventory rows.
pub fn inventory(target: &Target) -> Result<Vec<SurfaceEntry>, String> {
    Ok(current(target)?
        .into_iter()
        .map(|e| SurfaceEntry {
            path: e.id,
            kind: e.kind,
            signature: String::new(),
            pkg: target.label.clone(),
            summary: String::new(),
            docs: String::new(),
        })
        .collect())
}

/// The project's symbols as an `llms.txt` section.
pub fn llms_section(target: &Target) -> Result<ProjectSurface, String> {
    Ok(ProjectSurface {
        name: target.label.clone(),
        entries: current(target)?
            .into_iter()
            .map(|e| ProjectEntry {
                id: e.id,
                kind: e.kind,
                documented: e.documented,
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use soothfast_lang::Lang;

    use super::*;

    fn repo(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("soothfast-surf-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("web")).unwrap();
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "t@example.com"],
            &["config", "user.name", "t"],
        ] {
            invoke::git_in(&dir, args).unwrap();
        }
        dir
    }

    fn commit(dir: &Path, file: &str, text: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        invoke::git_in(dir, &["add", "-A"]).unwrap();
        invoke::git_in(dir, &["commit", "-q", "-m", "c"]).unwrap();
    }

    #[test]
    fn the_diff_reports_symbols_added_and_removed_since_a_ref() {
        let dir = repo("diff");
        commit(
            &dir,
            "web/a.ts",
            "export function keep() {}\nexport function gone() {}\nfunction local() {}\n",
        );
        invoke::git_in(&dir, &["tag", "v1"]).unwrap();
        commit(
            &dir,
            "web/a.ts",
            "export function keep() {}\nexport function fresh() {}\nfunction other() {}\n",
        );
        let target = Target {
            label: "web".into(),
            dir: dir.join("web"),
            langs: vec![Lang::TypeScript],
        };
        let text = diff_text(&dir, &target, "v1").unwrap();
        assert_eq!(text, "ADDED    web/a.ts#fresh\nREMOVED  web/a.ts#gone\n");
        assert_eq!(diff_text(&dir, &target, "HEAD").unwrap(), "");
    }

    #[test]
    fn a_project_missing_at_the_ref_is_all_added() {
        let dir = repo("new");
        commit(&dir, "README.md", "x\n");
        invoke::git_in(&dir, &["tag", "v1"]).unwrap();
        commit(&dir, "web/a.ts", "export function fresh() {}\n");
        let target = Target {
            label: "web".into(),
            dir: dir.join("web"),
            langs: vec![Lang::TypeScript],
        };
        assert_eq!(
            diff_text(&dir, &target, "v1").unwrap(),
            "ADDED    web/a.ts#fresh\n"
        );
    }
}
