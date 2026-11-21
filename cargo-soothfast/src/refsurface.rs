//! The public surface of a fixed commit, kept in `.soothfast/runs` beside the
//! stored report so the next regeneration reads it instead of compiling
//! rustdoc for the release again.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use soothfast_docs::surface::{ItemInfo, Surface};

use crate::invoke::{self, Visibility};
use crate::runcache;

/// The key a commit's surface is kept under: the commit, the package, the
/// features, the visibility, and the toolchain and code that turn it into a
/// surface.
pub fn key(
    sha: &str,
    pkg: &str,
    features: Option<&str>,
    visibility: Visibility,
    rustc: &str,
) -> String {
    runcache::hashed(&[
        "surface".to_string(),
        sha.to_string(),
        pkg.to_string(),
        features.unwrap_or("").to_string(),
        format!("{visibility:?}"),
        rustc.to_string(),
        invoke::RUSTDOC_FORMAT_VERSION.to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
    ])
}

/// The surface kept under `key`, or `build`'s, which is then kept. The
/// bool says whether it was stored.
pub fn reuse(
    key: Option<&str>,
    build: impl FnOnce() -> Result<Surface, String>,
) -> Result<(Surface, bool), String> {
    if let Some(surface) = key
        .and_then(runcache::stored_doc)
        .and_then(|doc| from_value(&doc))
    {
        return Ok((surface, true));
    }
    let surface = build()?;
    if let Some(key) = key {
        runcache::keep_doc(key, &to_value(&surface));
    }
    Ok((surface, false))
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
    use std::cell::Cell;

    use soothfast_docs::surface::{ItemInfo, Surface};

    use super::{Visibility, from_value, key, reuse, to_value};
    use crate::runcache;

    fn surface() -> Surface {
        let item = |kind: &str, fingerprint: u64| ItemInfo {
            kind: kind.into(),
            fingerprint,
            signature: format!("pub {kind} x"),
            has_docs: fingerprint.is_multiple_of(2),
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

    fn forget(key: &str) {
        if let Some(dir) = runcache::state_dir("runs") {
            let _ = std::fs::remove_file(dir.join(format!("{key}.json")));
        }
    }

    #[test]
    fn a_surface_survives_the_store() {
        let s = surface();
        assert!(same(&from_value(&to_value(&s)).expect("surface"), &s));
        assert!(from_value(&serde_json::json!({ "items": 3 })).is_none());
    }

    #[test]
    fn a_stored_surface_is_used_without_building() {
        let key = key(
            "test-refsurface-hit",
            "demo",
            None,
            Visibility::Public,
            "rustc 1",
        );
        let built = Cell::new(0);
        let build = || {
            built.set(built.get() + 1);
            Ok(surface())
        };
        let (first, stored) = reuse(Some(&key), build).unwrap();
        assert!(!stored && built.get() == 1);
        let (second, stored) = reuse(Some(&key), build).unwrap();
        assert!(stored && built.get() == 1, "built {} times", built.get());
        assert!(same(&first, &second));
        forget(&key);
    }

    #[test]
    fn a_different_toolchain_features_or_visibility_misses() {
        let at = |features, visibility, rustc| key("aaa", "demo", features, visibility, rustc);
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
    fn a_build_without_a_key_is_not_kept() {
        let (_, stored) = reuse(None, || Ok(surface())).unwrap();
        assert!(!stored);
    }
}
