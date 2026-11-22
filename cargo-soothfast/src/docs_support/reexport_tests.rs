//! A facade keeps the surface it had when its items move to a sibling crate
//! and are re-exported at the same paths. Builds real rustdoc JSON, so it
//! needs the rustdoc toolchain.

use std::path::{Path, PathBuf};

use soothfast_docs::surface::{ItemInfo, Surface};

use super::{DocsText, surface_in};

const MANIFEST_A: &str = "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nextra = [\"b/extra\"]\n\n[dependencies]\nb = { path = \"../b\" }\nc = { path = \"../c\" }\n";
const MANIFEST_B: &str =
    "[package]\nname = \"b\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\nextra = []\n";

const INDICATORS: &str = "//! Indicators.\n\n/// Simple moving average.\npub fn sma(x: &[f64]) -> f64 {\n    x.iter().sum::<f64>() / x.len() as f64\n}\n";

const ITEMS: &str = r#"pub mod indicators;

mod prelude {
    /// One.
    pub fn one() -> u8 {
        1
    }

    /// Two.
    pub struct Two;
}
pub use prelude::*;

/// Doubles.
pub fn named(x: u8) -> u8 {
    x * 2
}

/// Counts.
pub struct Counter {
    pub n: u8,
}

impl Counter {
    /// A zeroed counter.
    pub fn new() -> Self {
        Counter { n: 0 }
    }

    pub const MAX: u8 = 9;
}

/// Gated.
#[cfg(feature = "extra")]
pub fn gated() {}
"#;

const FACADE: &str = r#"pub use b::indicators;
pub use b::named;
pub use b::Counter;
pub use b::prelude_items::*;
#[cfg(feature = "extra")]
pub use b::gated;
"#;

const NOT_A_MEMBER: &str = "pub use c::outside;\n";

const REEXPORTED: [&str; 7] = [
    "a::indicators",
    "a::indicators::sma",
    "a::named",
    "a::Counter",
    "a::one",
    "a::Two",
    "a::gated",
];
const MEMBERS: [&str; 2] = ["a::Counter::new", "a::Counter::MAX"];

fn workspace(layout: &str, a: &str, b: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "soothfast-reexport-{layout}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let files = [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"a\", \"b\"]\nexclude = [\"c\"]\nresolver = \"2\"\n",
        ),
        (
            "c/Cargo.toml",
            "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
        ),
        (
            "c/src/lib.rs",
            "/// Outside the workspace.\npub fn outside() {}\n",
        ),
        ("a/Cargo.toml", MANIFEST_A),
        ("b/Cargo.toml", MANIFEST_B),
        ("a/src/lib.rs", a),
        ("b/src/lib.rs", b),
        ("a/src/indicators.rs", INDICATORS),
        ("b/src/indicators.rs", INDICATORS),
    ];
    for (rel, text) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    root
}

fn surface_of(root: &Path) -> (Surface, DocsText) {
    let (surface, docs, _) = surface_in("a", Some(root), Some("extra")).unwrap();
    (surface, docs)
}

fn facts(info: &ItemInfo) -> (&str, u64, &str, bool) {
    (
        info.kind.as_str(),
        info.fingerprint,
        info.signature.as_str(),
        info.has_docs,
    )
}

#[test]
#[ignore = "builds rustdoc JSON with the nightly toolchain"]
fn moving_items_to_a_path_dependency_keeps_the_facade_surface() {
    // Built one after the other: under a shared CARGO_TARGET_DIR the two
    // workspaces' `b` units collide, and only a newer mtime rebuilds it.
    let local_root = workspace("local", &format!("{ITEMS}{NOT_A_MEMBER}"), "//! Empty.\n");
    let (local, local_docs) = surface_of(&local_root);
    let moved_root = workspace(
        "moved",
        &format!("{FACADE}{NOT_A_MEMBER}"),
        &ITEMS
            .replace("mod prelude", "pub mod prelude_items")
            .replace("pub use prelude::*;", "pub use prelude_items::*;"),
    );
    let (moved, moved_docs) = surface_of(&moved_root);

    for path in REEXPORTED {
        let (Some(was), Some(now)) = (local.items.get(path), moved.items.get(path)) else {
            panic!(
                "{path}: local {:?}, moved {:?}",
                local.items.keys(),
                moved.items.keys()
            );
        };
        assert_eq!(facts(was), facts(now), "{path}");
        assert_ne!(now.fingerprint, 0, "{path}");
        if let Some(text) = local_docs.get(path) {
            assert_eq!(moved_docs.get(path), Some(text), "{path}");
        }
    }
    for path in MEMBERS {
        assert_eq!(
            facts(&local.members[path]),
            facts(&moved.members[path]),
            "{path}"
        );
    }
    assert_eq!(local.members.len(), moved.members.len());
    assert_eq!(local.grouped().len(), moved.grouped().len());
    assert!(moved.items.keys().all(|k| local.items.contains_key(k)));
    assert!(!moved.items.keys().any(|k| k.starts_with("b::")));
    assert!(!local.items.contains_key("a::outside"));
    assert!(!moved.items.contains_key("a::outside"));

    let _ = std::fs::remove_dir_all(&local_root);
    let _ = std::fs::remove_dir_all(&moved_root);
}
