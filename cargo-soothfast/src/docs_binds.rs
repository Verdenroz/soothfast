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

/// Current fingerprint of a bind target. `surfaces` pairs each `-p` package
/// with its rustdoc surface; it is empty unless the scanned docs bind a Rust item.
pub fn resolve(root: &Path, surfaces: &[(String, Surface)], item: &str) -> Result<String, String> {
    match classify(item) {
        Target::File { path, symbol } => file_fingerprint(root, path, symbol),
        Target::Rust(item) => rust_fingerprint(surfaces, item),
    }
}

/// A Rust bind resolves against the package its first path segment names.
fn rust_fingerprint(surfaces: &[(String, Surface)], item: &str) -> Result<String, String> {
    let krate = item.split("::").next().unwrap_or(item).replace('-', "_");
    let (_, surf) = surfaces
        .iter()
        .find(|(pkg, _)| pkg.replace('-', "_") == krate)
        .ok_or_else(|| format!("crate `{krate}` is not among the -p packages; add `-p {krate}`"))?;
    surf.lookup(item)
        .map(|info| format!("{:016x}", info.fingerprint))
        .ok_or_else(|| "item not found in crate".into())
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

    use soothfast_docs::surface::ItemInfo;

    use super::*;

    fn surface_with(path: &str) -> Surface {
        let mut surface = Surface::default();
        surface.items.insert(
            path.to_string(),
            ItemInfo {
                kind: "function".into(),
                fingerprint: 0xabc,
                signature: String::new(),
                has_docs: false,
            },
        );
        surface
    }

    #[test]
    fn rust_bind_resolves_against_the_package_its_first_segment_names() {
        let surfaces = [
            ("alpha-core".to_string(), surface_with("alpha_core::f")),
            ("beta".to_string(), surface_with("beta::g")),
        ];
        let root = Path::new(".");
        assert_eq!(
            resolve(root, &surfaces, "alpha_core::f").unwrap(),
            "0000000000000abc"
        );
        assert_eq!(
            resolve(root, &surfaces, "beta::g").unwrap(),
            "0000000000000abc"
        );
        assert_eq!(
            resolve(root, &surfaces, "beta::f").unwrap_err(),
            "item not found in crate"
        );
    }

    #[test]
    fn rust_bind_to_a_crate_outside_the_packages_says_to_add_it() {
        let surfaces = [("beta".to_string(), surface_with("beta::g"))];
        assert_eq!(
            resolve(Path::new("."), &surfaces, "gamma::g").unwrap_err(),
            "crate `gamma` is not among the -p packages; add `-p gamma`"
        );
    }

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
