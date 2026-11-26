//! `#[soothfast::fixture]` items no bench references. Read from source, since
//! the registry lists fixtures but not who uses them, and a bench names its
//! fixture only in `setup` or `setup_sized`.

use std::collections::BTreeMap;
use std::path::Path;

/// A warning per fixture in `sources` (path, text) whose name appears nowhere
/// else in `sources`: a bench that used it would name it in `setup`,
/// `setup_sized` or a call.
pub fn unreferenced(sources: &[(String, String)], pkg: &str) -> Vec<String> {
    let defined: Vec<_> = sources
        .iter()
        .flat_map(|(path, text)| fixture_names(text).into_iter().map(move |n| (path, n)))
        .collect();
    let mut uses: BTreeMap<&str, usize> = defined.iter().map(|(_, n)| (*n, 0)).collect();
    for (_, text) in sources {
        for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            if let Some(count) = uses.get_mut(word) {
                *count += 1;
            }
        }
    }
    defined
        .iter()
        .filter(|(_, name)| uses[name] <= 1)
        .map(|(path, name)| {
            format!("warning: fixture {name} in {path} is referenced by no bench of {pkg}")
        })
        .collect()
}

fn fixture_names(text: &str) -> Vec<&str> {
    let mut names = Vec::new();
    for (at, _) in text.match_indices("fixture") {
        let attribute = text[..at].ends_with("#[") || text[..at].ends_with("soothfast::");
        let closed = text[at + "fixture".len()..].starts_with([']', '(']);
        if !(attribute && closed) {
            continue;
        }
        if let Some(rest) = text[at..].split_once("fn ").map(|(_, rest)| rest) {
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            names.push(&rest[..end]);
        }
    }
    names
}

/// Every `.rs` file under `pkg_dir`'s `src` and `benches`, as (path relative
/// to `pkg_dir`, text).
pub fn sources(pkg_dir: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for dir in ["src", "benches"] {
        collect(pkg_dir, &pkg_dir.join(dir), &mut found);
    }
    found
}

fn collect(root: &Path, dir: &Path, found: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect(root, &path, found);
        } else if path.extension().is_some_and(|e| e == "rs")
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            found.push((rel.display().to_string(), text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(files: &[(&str, &str)]) -> Vec<(String, String)> {
        files
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect()
    }

    #[test]
    fn a_fixture_no_bench_names_is_warned_about() {
        let files = sources(&[(
            "benches/soothfast.rs",
            "#[fixture]\nfn used() -> u8 { 1 }\n\n#[soothfast::fixture]\nfn idle() -> u8 { 2 }\n\n#[bench(setup = used)]\nfn b(_: &u8) {}\n",
        )]);
        assert_eq!(
            unreferenced(&files, "demo"),
            ["warning: fixture idle in benches/soothfast.rs is referenced by no bench of demo"]
        );
    }

    #[test]
    fn a_bench_in_another_file_references_a_fixture() {
        let files = sources(&[
            ("src/lib.rs", "#[soothfast::fixture]\npub fn input() {}\n"),
            (
                "benches/soothfast.rs",
                "#[bench(setup_sized = input)]\nfn b() {}\n",
            ),
        ]);
        assert!(unreferenced(&files, "demo").is_empty());
    }

    #[test]
    fn a_name_that_only_contains_the_fixture_name_is_no_reference() {
        let files = sources(&[(
            "benches/soothfast.rs",
            "#[fixture]\nfn input() {}\n#[bench(setup = input_big)]\nfn b() {}\n",
        )]);
        assert_eq!(unreferenced(&files, "demo").len(), 1);
    }
}
