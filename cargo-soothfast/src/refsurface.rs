//! The public surface of a fixed commit, kept in `.soothfast/runs` beside the
//! stored report so the next regeneration reads it instead of compiling
//! rustdoc for the release again.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use soothfast_docs::surface::{ItemInfo, Surface};

use crate::invoke::{self, Visibility};
use crate::runcache;

/// The key a commit's surface is kept under: the commit, the package, the
/// features, the visibility, `build` (the rustdoc toolchain and build
/// environment, see [`invoke::rustdoc_build`]) and the code that turns
/// rustdoc JSON into a surface.
pub fn key(
    sha: &str,
    pkg: &str,
    features: Option<&str>,
    visibility: Visibility,
    build: &str,
) -> String {
    runcache::hashed(&[
        "surface".to_string(),
        sha.to_string(),
        pkg.to_string(),
        features.unwrap_or("").to_string(),
        format!("{visibility:?}"),
        build.to_string(),
        invoke::RUSTDOC_FORMAT_VERSION.to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
    ])
}

/// The surface kept under `key`, when there is a key and the store holds it.
pub fn stored(key: Option<&str>) -> Option<Surface> {
    key.and_then(runcache::stored_doc)
        .and_then(|doc| from_value(&doc))
}

/// Keeps `surface` under `key`, when there is one.
pub fn keep(key: Option<&str>, surface: &Surface) {
    if let Some(key) = key {
        runcache::keep_doc(key, &to_value(surface));
    }
}

fn to_value(surface: &Surface) -> Value {
    let items = |items: &BTreeMap<String, ItemInfo>| {
        items
            .iter()
            .map(|(path, info)| {
                let item = json!({
                    "kind": info.kind,
                    "fingerprint": info.fingerprint,
                    "signature": info.signature,
                    "has_docs": info.has_docs,
                });
                (path.clone(), item)
            })
            .collect::<serde_json::Map<_, _>>()
    };
    json!({ "items": items(&surface.items), "members": items(&surface.members) })
}

fn from_value(doc: &Value) -> Option<Surface> {
    let items = |v: &Value| -> Option<BTreeMap<String, ItemInfo>> {
        v.as_object()?
            .iter()
            .map(|(path, item)| {
                let info = ItemInfo {
                    kind: item["kind"].as_str()?.to_string(),
                    fingerprint: item["fingerprint"].as_u64()?,
                    signature: item["signature"].as_str()?.to_string(),
                    has_docs: item["has_docs"].as_bool()?,
                    spans: Vec::new(),
                };
                Some((path.clone(), info))
            })
            .collect()
    };
    Some(Surface {
        items: items(&doc["items"])?,
        members: items(&doc["members"])?,
    })
}

#[cfg(test)]
mod tests {
    use soothfast_docs::surface::{ItemInfo, Surface};

    use super::{Visibility, from_value, keep, key, stored, to_value};
    use crate::runcache;

    fn surface() -> Surface {
        let item = |kind: &str, fingerprint: u64| ItemInfo {
            kind: kind.into(),
            fingerprint,
            signature: format!("pub {kind} x"),
            has_docs: fingerprint.is_multiple_of(2),
            spans: Vec::new(),
        };
        let mut s = Surface::default();
        s.items
            .insert("demo::run".into(), item("function", u64::MAX));
        s.members
            .insert("demo::Thing::go".into(), item("method", 8));
        s
    }

    fn same(a: &Surface, b: &Surface) -> bool {
        format!("{a:?}") == format!("{b:?}")
    }

    #[test]
    fn a_surface_survives_the_store() {
        let s = surface();
        assert!(same(&from_value(&to_value(&s)).expect("surface"), &s));
        assert!(from_value(&serde_json::json!({ "items": 3 })).is_none());
    }

    #[test]
    fn a_kept_surface_is_read_back_under_its_key() {
        let key = key(
            "test-refsurface-hit",
            "demo",
            None,
            Visibility::Public,
            "rustc 1",
        );
        assert!(stored(Some(&key)).is_none());
        keep(Some(&key), &surface());
        let read = stored(Some(&key)).expect("stored surface");
        assert!(same(&read, &surface()));
        runcache::forget(&key);
    }

    #[test]
    fn a_different_build_features_or_visibility_misses() {
        let at = |features, visibility, build| key("aaa", "demo", features, visibility, build);
        let base = at(None, Visibility::Public, "rustc 1");
        assert_ne!(base, at(None, Visibility::Public, "rustc 2"));
        assert_ne!(base, at(Some("full"), Visibility::Public, "rustc 1"));
        assert_ne!(base, at(None, Visibility::Private, "rustc 1"));
        assert_ne!(
            base,
            key("bbb", "demo", None, Visibility::Public, "rustc 1")
        );
        assert_ne!(
            base,
            key("aaa", "other", None, Visibility::Public, "rustc 1")
        );
    }

    #[test]
    fn a_surface_without_a_key_is_not_kept() {
        keep(None, &surface());
        assert!(stored(None).is_none());
    }
}
