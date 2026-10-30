use crate::{Lang, Symbol, fingerprint, symbols};

mod ecma;
mod java;
mod kotlin;
mod public;
mod python;

fn syms(lang: Lang, source: &str) -> Vec<Symbol> {
    symbols(lang, source).unwrap()
}

fn names(lang: Lang, source: &str) -> Vec<String> {
    syms(lang, source).into_iter().map(|s| s.name).collect()
}

fn fp(lang: Lang, source: &str, name: &str) -> Option<String> {
    fingerprint(lang, source, name)
}

#[test]
fn for_path_picks_language_by_extension() {
    use std::path::Path;
    assert_eq!(Lang::for_path(Path::new("a/b.py")), Some(Lang::Python));
    assert_eq!(Lang::for_path(Path::new("b.pyi")), Some(Lang::Python));
    for (file, lang) in [
        ("A.java", Lang::Java),
        ("a.kt", Lang::Kotlin),
        ("build.kts", Lang::Kotlin),
        ("a.js", Lang::JavaScript),
        ("a.mjs", Lang::JavaScript),
        ("a.cjs", Lang::JavaScript),
        ("a.jsx", Lang::JavaScript),
        ("a.ts", Lang::TypeScript),
        ("a.mts", Lang::TypeScript),
        ("a.cts", Lang::TypeScript),
        ("a.tsx", Lang::Tsx),
    ] {
        assert_eq!(Lang::for_path(Path::new(file)), Some(lang), "{file}");
    }
    assert_eq!(Lang::for_path(Path::new("b.rs")), None);
    assert_eq!(Lang::for_path(Path::new("Makefile")), None);
}
