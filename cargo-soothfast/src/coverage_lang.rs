//! `coverage docs` for source trees that are not Rust: which public symbols
//! carry a doc comment or docstring.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use soothfast_lang::{Lang, public_symbols};

use crate::init::ecosystem::{Ecosystem, parse_projects};

/// Directories that hold dependencies, build output or tests rather than the
/// documented surface.
const SKIPPED_DIRS: [&str; 8] = [
    "node_modules",
    "build",
    "target",
    "dist",
    "venv",
    "test",
    "tests",
    "__tests__",
];

/// A source directory and the languages whose files it contributes.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    /// The directory relative to the repository root, normalized: no `./`,
    /// no doubled or trailing `/`, empty for the root itself.
    pub label: String,
    /// The directory to read, resolved against the repository root.
    pub dir: PathBuf,
    /// The languages whose files count.
    pub langs: Vec<Lang>,
}

/// A public symbol and whether it is documented.
pub struct Entry {
    /// `path#Symbol`.
    pub id: String,
    /// Lowercase symbol kind (`function`, `class`).
    pub kind: String,
    /// Whether a doc comment or docstring is present.
    pub documented: bool,
}

/// Languages named by a `--lang` value: an ecosystem, or a single language
/// of one. Rust is covered by `-p`, so it names none.
pub fn langs_for(word: &str) -> Result<Vec<Lang>, String> {
    match (Ecosystem::parse(word), word) {
        (Ok(Ecosystem::Rust), _) => Err("rust is covered by -p, not --lang".into()),
        (Ok(eco), _) => Ok(eco.langs()),
        (_, "java") => Ok(vec![Lang::Java]),
        (_, "kotlin") => Ok(vec![Lang::Kotlin]),
        (_, "javascript") => Ok(vec![Lang::JavaScript]),
        (_, "typescript") => Ok(vec![Lang::TypeScript, Lang::Tsx]),
        _ => Err(format!(
            "unknown --lang {word:?} (gradle, maven, python, node, java, kotlin, javascript or typescript)"
        )),
    }
}

/// A directory as a repository-relative label: no `./`, no doubled or
/// trailing `/`, and empty for the root itself.
pub fn normalize_label(dir: &str) -> String {
    dir.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// The id of `rel` inside the directory `label`.
pub fn join_label(label: &str, rel: &str) -> String {
    if label.is_empty() {
        rel.to_string()
    } else {
        format!("{label}/{rel}")
    }
}

/// The trees to cover: `--dir` (repeatable) with `--lang`, else the
/// manifest's projects.
pub fn targets(root: &Path, dirs: &[String], lang: Option<&str>) -> Result<Vec<Target>, String> {
    match (dirs, lang) {
        ([], None) => manifest_targets(root),
        (dirs @ [_, ..], Some(word)) => {
            let langs = langs_for(word)?;
            Ok(dirs
                .iter()
                .map(|dir| Target {
                    label: normalize_label(dir),
                    dir: root.join(dir),
                    langs: langs.clone(),
                })
                .collect())
        }
        _ => Err("--dir and --lang go together".into()),
    }
}

/// The non-Rust `[[project]]` entries of `root`'s `soothfast.toml`, read with
/// the same parser as `init`. Only `dir` and `ecosystem` are used.
pub fn manifest_targets(root: &Path) -> Result<Vec<Target>, String> {
    let path = root.join("soothfast.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let mut targets = Vec::new();
    for project in parse_projects(&text)? {
        let Some(ecosystem) = project.settings.ecosystem.filter(|e| *e != Ecosystem::Rust) else {
            continue;
        };
        targets.push(Target {
            label: normalize_label(&project.dir),
            dir: root.join(&project.dir),
            langs: ecosystem.langs(),
        });
    }
    Ok(targets)
}

/// Every public symbol under `target`, one entry per file and name, in path
/// order, ids relative to the target's directory.
pub fn entries(target: &Target) -> Result<Vec<Entry>, String> {
    let mut files = Vec::new();
    collect(&target.dir, &target.langs, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new();
    for (path, lang) in files {
        let source = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let shown = path.strip_prefix(&target.dir).unwrap_or(&path);
        out.extend(file_entries(&shown.display().to_string(), lang, &source)?);
    }
    Ok(out)
}

/// The public symbols of one file, ids `shown#Name`. A name with several
/// overloads is documented when any overload is.
pub fn file_entries(shown: &str, lang: Lang, source: &str) -> Result<Vec<Entry>, String> {
    let symbols = public_symbols(lang, source).map_err(|e| format!("{shown}: {e}"))?;
    let mut by_name: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for s in symbols {
        let slot = by_name
            .entry(s.name)
            .or_insert_with(|| (format!("{:?}", s.kind).to_lowercase(), false));
        slot.1 |= s.documented;
    }
    Ok(by_name
        .into_iter()
        .map(|(name, (kind, documented))| Entry {
            id: format!("{shown}#{name}"),
            kind,
            documented,
        })
        .collect())
}

/// The language of `rel` when a target for `langs` covers the file: the
/// extension matches and no directory on the way is skipped.
pub fn covered(rel: &Path, langs: &[Lang]) -> Option<Lang> {
    let dirs = rel.parent()?.components();
    let hidden_or_skipped = dirs.filter_map(|c| c.as_os_str().to_str()).any(skipped_dir);
    if hidden_or_skipped {
        return None;
    }
    Lang::for_path(rel).filter(|l| langs.contains(l))
}

fn skipped_dir(name: &str) -> bool {
    name.starts_with('.') || SKIPPED_DIRS.contains(&name)
}

/// Symlinks are skipped: git records them as links, and a cycle would never
/// end.
fn collect(dir: &Path, langs: &[Lang], out: &mut Vec<(PathBuf, Lang)>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_str().unwrap_or("");
        if kind.is_dir() {
            if !skipped_dir(name) {
                collect(&path, langs, out)?;
            }
        } else if kind.is_file()
            && let Some(lang) = Lang::for_path(&path).filter(|l| langs.contains(l))
        {
            out.push((path, lang));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("soothfast-cov-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (path, text) in files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
        dir
    }

    #[test]
    fn manifest_reads_only_dir_and_ecosystem_and_skips_rust() {
        let dir = fixture(
            "manifest",
            &[(
                "soothfast.toml",
                "[site]\nname = \"x\"\n\n[[project]]\ndir = \"core\"\necosystem = \"rust\"\n\n[[project]]\ndir = \"client\"\necosystem = \"gradle\"\nfuture = { a = 1 }\n\n[[project]]\ndir = \"web/\"\necosystem = \"node\"\n",
            )],
        );
        let targets = manifest_targets(&dir).unwrap();
        assert_eq!(
            targets,
            [
                Target {
                    label: "client".into(),
                    dir: dir.join("client"),
                    langs: vec![Lang::Kotlin, Lang::Java]
                },
                Target {
                    label: "web".into(),
                    dir: dir.join("web/"),
                    langs: vec![Lang::JavaScript, Lang::TypeScript, Lang::Tsx]
                },
            ]
        );
    }

    #[test]
    fn labels_drop_dot_segments_and_extra_slashes() {
        assert_eq!(normalize_label("."), "");
        assert_eq!(normalize_label("./web/"), "web");
        assert_eq!(normalize_label("a//b/./c"), "a/b/c");
        assert_eq!(join_label("", "a.py"), "a.py");
        assert_eq!(join_label("web", "a.py"), "web/a.py");
    }

    #[test]
    fn entries_cover_public_symbols_and_skip_tests_and_other_languages() {
        let dir = fixture(
            "entries",
            &[
                (
                    "src/app.py",
                    "def shown():\n    \"\"\"Doc.\"\"\"\n\ndef bare():\n    pass\n\ndef _hidden():\n    pass\n",
                ),
                ("tests/t.py", "def test_x():\n    pass\n"),
                ("src/Other.java", "public class Other {}\n"),
            ],
        );
        let target = Target {
            label: String::new(),
            dir: dir.clone(),
            langs: vec![Lang::Python],
        };
        let got: Vec<(String, bool)> = entries(&target)
            .unwrap()
            .into_iter()
            .map(|e| (e.id, e.documented))
            .collect();
        assert_eq!(
            got,
            [
                ("src/app.py#bare".into(), false),
                ("src/app.py#shown".into(), true)
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_skipped_so_a_cycle_ends() {
        let dir = fixture("links", &[("src/a.py", "def a():\n    pass\n")]);
        std::os::unix::fs::symlink(&dir, dir.join("src/loop")).unwrap();
        std::os::unix::fs::symlink(dir.join("src/a.py"), dir.join("src/b.py")).unwrap();
        let target = Target {
            label: String::new(),
            dir,
            langs: vec![Lang::Python],
        };
        let ids: Vec<String> = entries(&target)
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(ids, ["src/a.py#a"]);
    }

    #[test]
    fn covered_skips_hidden_and_vendored_directories() {
        let langs = [Lang::Python];
        assert_eq!(covered(Path::new("src/a.py"), &langs), Some(Lang::Python));
        assert_eq!(covered(Path::new("a.py"), &langs), Some(Lang::Python));
        assert_eq!(covered(Path::new("tests/a.py"), &langs), None);
        assert_eq!(covered(Path::new(".venv/lib/a.py"), &langs), None);
        assert_eq!(covered(Path::new("src/A.java"), &langs), None);
    }

    #[test]
    fn lang_words_name_ecosystems_or_languages() {
        assert_eq!(langs_for("gradle").unwrap(), [Lang::Kotlin, Lang::Java]);
        assert_eq!(langs_for("kotlin").unwrap(), [Lang::Kotlin]);
        assert!(langs_for("rust").unwrap_err().contains("-p"));
        assert!(langs_for("cobol").unwrap_err().contains("kotlin"));
    }
}
