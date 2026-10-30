//! `coverage docs` for source trees that are not Rust: which public symbols
//! carry a doc comment or docstring.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use soothfast_lang::{Lang, public_symbols};
use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

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
    pub dir: PathBuf,
    pub langs: Vec<Lang>,
}

/// A public symbol and whether it is documented.
pub struct Entry {
    pub id: String,
    pub kind: String,
    pub documented: bool,
}

/// Languages named by a `--lang` value: a language, or the ecosystem that
/// builds one. Rust is covered by `-p`, so it names none.
pub fn langs_for(word: &str) -> Result<Vec<Lang>, String> {
    match word {
        "java" => Ok(vec![Lang::Java]),
        "kotlin" => Ok(vec![Lang::Kotlin]),
        "python" => Ok(vec![Lang::Python]),
        "javascript" => Ok(vec![Lang::JavaScript]),
        "typescript" => Ok(vec![Lang::TypeScript, Lang::Tsx]),
        "gradle" => Ok(vec![Lang::Kotlin, Lang::Java]),
        "maven" => Ok(vec![Lang::Java]),
        "node" => Ok(vec![Lang::JavaScript, Lang::TypeScript, Lang::Tsx]),
        other => Err(format!(
            "unknown --lang {other:?} (java, kotlin, python, javascript, typescript, gradle, maven, node)"
        )),
    }
}

/// The non-Rust `[[project]]` entries of `root`'s `soothfast.toml`. Only
/// `dir` and `ecosystem` are read; every other key and table is ignored.
pub fn manifest_targets(root: &Path) -> Result<Vec<Target>, String> {
    let path = root.join("soothfast.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let mut projects: Vec<(Option<String>, Option<String>)> = Vec::new();
    let mut in_project = false;
    for (_, line) in logical_lines(&text) {
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_project = inner.trim() == "[project]";
            if in_project {
                projects.push((None, None));
            }
            continue;
        }
        let (Some((key, value)), Some(project)) = (line.split_once('='), projects.last_mut())
        else {
            continue;
        };
        if !in_project {
            continue;
        }
        if let Ok(TomlValue::Str(value)) = parse_value(value.trim()) {
            match key.trim() {
                "dir" => project.0 = Some(value),
                "ecosystem" => project.1 = Some(value),
                _ => {}
            }
        }
    }
    let mut targets = Vec::new();
    for (dir, ecosystem) in projects {
        let (Some(dir), Some(ecosystem)) = (dir, ecosystem) else {
            continue;
        };
        if ecosystem == "rust" {
            continue;
        }
        targets.push(Target {
            dir: root.join(dir),
            langs: langs_for(&ecosystem)?,
        });
    }
    Ok(targets)
}

/// Every public symbol under `target`, one entry per file and name, in path
/// order. A name with several overloads is documented when any overload is.
pub fn entries(target: &Target) -> Result<Vec<Entry>, String> {
    let mut files = Vec::new();
    collect(&target.dir, &target.langs, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new();
    for (path, lang) in files {
        let source = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let symbols =
            public_symbols(lang, &source).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut by_name: BTreeMap<String, (String, bool)> = BTreeMap::new();
        for s in symbols {
            let slot = by_name
                .entry(s.name)
                .or_insert_with(|| (format!("{:?}", s.kind).to_lowercase(), false));
            slot.1 |= s.documented;
        }
        let shown = path.strip_prefix(&target.dir).unwrap_or(&path);
        for (name, (kind, documented)) in by_name {
            out.push(Entry {
                id: format!("{}#{name}", shown.display()),
                kind,
                documented,
            });
        }
    }
    Ok(out)
}

fn collect(dir: &Path, langs: &[Lang], out: &mut Vec<(PathBuf, Lang)>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !name.starts_with('.') && !SKIPPED_DIRS.contains(&name) {
                collect(&path, langs, out)?;
            }
        } else if let Some(lang) = Lang::for_path(&path).filter(|l| langs.contains(l)) {
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
                "[site]\nname = \"x\"\n\n[[project]]\ndir = \"core\"\necosystem = \"rust\"\n\n[[project]]\ndir = \"client\"\necosystem = \"gradle\"\nfuture = { a = 1 }\n\n[[project]]\ndir = \"web\"\necosystem = \"node\"\n",
            )],
        );
        let targets = manifest_targets(&dir).unwrap();
        assert_eq!(
            targets,
            [
                Target {
                    dir: dir.join("client"),
                    langs: vec![Lang::Kotlin, Lang::Java]
                },
                Target {
                    dir: dir.join("web"),
                    langs: vec![Lang::JavaScript, Lang::TypeScript, Lang::Tsx]
                },
            ]
        );
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

    #[test]
    fn an_unknown_lang_names_the_choices() {
        assert!(langs_for("cobol").unwrap_err().contains("kotlin"));
    }
}
