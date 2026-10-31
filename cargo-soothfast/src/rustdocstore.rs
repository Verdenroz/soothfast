//! Rustdoc JSON kept on disk, keyed by everything that produces it.
//!
//! Building it means compiling the package and every dependency under
//! nightly, which dwarfs every step after it, and none of it is shared with
//! another job or run. The key and the entries follow `store`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::invoke::{self, Visibility};
use crate::runcache;
use crate::store::{self, Inputs};

/// Bytes kept before the least recently used entries are dropped.
pub(crate) const MAX_BYTES: u64 = 1 << 30;

/// One extraction, as far as the key is concerned.
pub struct Request<'a> {
    pub pkg: &'a str,
    pub features: Option<&'a str>,
    pub visibility: Visibility,
    /// `rustc -vV` of the toolchain that documents it.
    pub rustc: &'a str,
    pub env: &'a [(String, String)],
}

/// The key for `req` built from the tree at `root`, whose package sources
/// live in `crate_dirs`. `None` when part of the input cannot be read.
pub fn key(root: &Path, crate_dirs: &[PathBuf], req: &Request) -> Option<String> {
    let head = format!(
        "{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}",
        req.pkg,
        req.features.unwrap_or(""),
        req.visibility == Visibility::Private,
        invoke::RUSTDOC_FORMAT_VERSION,
        req.rustc,
    );
    store::cheap_key(root, crate_dirs, &head, req.env)
}

/// The document stored under `key` whose inputs still hold what they did.
pub fn load(key: &str, root: &Path) -> Option<Value> {
    load_from(&runcache::state_dir("rustdoc")?, key, root)
}

/// Whether a document is stored under `key` and its inputs still hold.
pub fn holds(key: &str, root: &Path) -> bool {
    runcache::state_dir("rustdoc")
        .is_some_and(|dir| store::lookup(&dir, key, root, "json").is_some())
}

/// Keep the JSON rustdoc wrote at `json` for a build that started at
/// `started`, unless the tree was edited meanwhile: `key` must still be the
/// key the tree computes (`key_now`), and no input may be newer than the build.
pub fn store(
    key: &str,
    key_now: Option<String>,
    root: &Path,
    inputs: &Inputs,
    started: SystemTime,
    json: &Path,
) {
    if key_now.as_deref() != Some(key) {
        return;
    }
    if let Some(dir) = runcache::state_dir("rustdoc") {
        store_in(&dir, key, root, inputs, started, json);
    }
}

fn load_from(dir: &Path, key: &str, root: &Path) -> Option<Value> {
    let path = store::lookup(dir, key, root, "json")?;
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn store_in(dir: &Path, key: &str, root: &Path, inputs: &Inputs, started: SystemTime, json: &Path) {
    if store::insert(dir, key, root, inputs, started, json, "json").is_some() {
        store::prune(dir, MAX_BYTES);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use super::*;
    use crate::store::testing::{Repo, inputs};

    fn request() -> Request<'static> {
        Request {
            pkg: "app",
            features: None,
            visibility: Visibility::Private,
            rustc: "rustc 1.99.0-nightly",
            env: &[],
        }
    }

    fn key_with(repo: &Repo, req: &Request) -> String {
        key(&repo.0, &repo.dirs(), req).unwrap()
    }

    #[test]
    fn the_key_follows_features_visibility_toolchain_and_flags() {
        let repo = Repo::new("rd-request");
        let base = key_with(&repo, &request());
        let flags = [("RUSTFLAGS".to_string(), "--cfg x".to_string())];
        let variants = [
            Request {
                features: Some("a"),
                ..request()
            },
            Request {
                visibility: Visibility::Public,
                ..request()
            },
            Request {
                rustc: "rustc 1.99.1-nightly",
                ..request()
            },
            Request {
                env: &flags,
                ..request()
            },
            Request {
                pkg: "dep",
                ..request()
            },
        ];
        let keys: BTreeSet<_> = variants.iter().map(|r| key_with(&repo, r)).collect();
        assert_eq!(keys.len(), variants.len());
        assert!(!keys.contains(&base));
    }

    #[test]
    fn a_stored_document_reads_back_identical() {
        let repo = Repo::new("rd-roundtrip");
        let json = repo.0.join("app.json");
        let text = r#"{"format_version":61,"index":{"0":{"name":"app"}}}"#;
        std::fs::write(&json, text).unwrap();
        let dir = repo.store_dir();
        assert_eq!(load_from(&dir, "abc", &repo.0), None);
        let future = SystemTime::now() + Duration::from_secs(3600);
        store_in(&dir, "abc", &repo.0, &inputs(&[]), future, &json);
        let expected: Value = serde_json::from_str(text).unwrap();
        assert_eq!(load_from(&dir, "abc", &repo.0), Some(expected));
    }
}
