//! Resolving `soothfast:bind` targets that name a symbol in a source file
//! (`path/to/file.py#Class.method`) rather than a Rust item.

use std::io::ErrorKind;
use std::path::Path;

use soothfast_docs::surface::Surface;
use soothfast_lang::Lang;

/// What a bind marker's target refers to.
#[derive(Debug, PartialEq, Eq)]
pub enum Target<'a> {
    Rust(&'a str),
    File { path: &'a str, symbol: &'a str },
}

pub fn classify(item: &str) -> Target<'_> {
    match item.split_once('#') {
        Some((path, symbol)) => Target::File { path, symbol },
        None => Target::Rust(item),
    }
}

/// Current fingerprint of a bind target. `surf` is the rustdoc surface, which
/// exists whenever the scanned docs bind at least one Rust item.
pub fn resolve(root: &Path, surf: Option<&Surface>, item: &str) -> Result<String, String> {
    match classify(item) {
        Target::File { path, symbol } => file_fingerprint(root, path, symbol),
        Target::Rust(item) => surf
            .and_then(|s| s.items.get(item))
            .map(|info| format!("{:016x}", info.fingerprint))
            .ok_or_else(|| "item not found in crate".into()),
    }
}

/// Fingerprint of `symbol` in `root/path`, or the reason it cannot be had.
pub fn file_fingerprint(root: &Path, path: &str, symbol: &str) -> Result<String, String> {
    let lang = Lang::for_path(Path::new(path))
        .ok_or_else(|| format!("unsupported language for {path}"))?;
    let source = match std::fs::read_to_string(root.join(path)) {
        Ok(s) => s,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err("file not found".into()),
        Err(e) => return Err(format!("cannot read {path}: {e}")),
    };
    soothfast_lang::fingerprint(lang, &source, symbol)
        .ok_or_else(|| format!("symbol not found in {path}"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixture(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("soothfast-binds-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (path, text) in files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
        dir
    }

    const PY: &str = "class Parser:\n    def parse(self, s):\n        return s.strip()\n";
    const TS: &str = "export function add(a: number, b: number): number {\n  return a + b;\n}\n";

    #[test]
    fn hash_makes_a_file_bind_and_nothing_else_does() {
        assert_eq!(classify("a::b::c"), Target::Rust("a::b::c"));
        assert_eq!(
            classify("src/app/util.py#Parser.parse"),
            Target::File {
                path: "src/app/util.py",
                symbol: "Parser.parse"
            }
        );
    }

    #[test]
    fn resolves_python_and_typescript_symbols() {
        let dir = fixture("ok", &[("src/u.py", PY), ("web/a-b.ts", TS)]);
        let py = file_fingerprint(&dir, "src/u.py", "Parser.parse").unwrap();
        let ts = file_fingerprint(&dir, "web/a-b.ts", "add").unwrap();
        assert_eq!(py.len(), 16);
        assert_eq!(ts.len(), 16);
        assert_ne!(py, ts);
    }

    #[test]
    fn body_edit_changes_the_fingerprint_and_comment_edit_does_not() {
        let before = fixture("edit-a", &[("u.py", PY)]);
        let comment = fixture(
            "edit-b",
            &[(
                "u.py",
                "class Parser:\n    def parse(self, s):\n        # trim\n        return s.strip()\n",
            )],
        );
        let body = fixture(
            "edit-c",
            &[(
                "u.py",
                "class Parser:\n    def parse(self, s):\n        return s.lstrip()\n",
            )],
        );
        let fp = |d: &PathBuf| file_fingerprint(d, "u.py", "Parser.parse").unwrap();
        assert_eq!(fp(&before), fp(&comment));
        assert_ne!(fp(&before), fp(&body));
    }

    #[test]
    fn reports_each_failure_reason() {
        let dir = fixture("fail", &[("u.py", PY), ("notes.txt", "x")]);
        let err = |path, sym| file_fingerprint(&dir, path, sym).unwrap_err();
        assert_eq!(err("u.py", "Parser.nope"), "symbol not found in u.py");
        assert_eq!(err("gone.py", "f"), "file not found");
        assert_eq!(err("notes.txt", "f"), "unsupported language for notes.txt");
    }
}
