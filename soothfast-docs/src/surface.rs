//! Public API surface from rustdoc JSON: item paths, span-derived
//! fingerprints, doc presence, and literal source signatures.
//!
//! Fingerprints hash the item's *source span text* (whitespace-normalized),
//! not rustdoc's structured types — rustdoc item ids shift across builds and
//! would churn a structure-based hash.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde_json::Value;

/// What the surface records about one public item — enough to detect any
/// change to it (fingerprint), classify the change (signature), and audit
/// coverage (kind, has_docs).
#[derive(Debug, Clone)]
pub struct ItemInfo {
    /// Rustdoc item kind (`function`, `struct`, `module`, ...).
    pub kind: String,
    /// FNV-1a of the whitespace-normalized span source.
    pub fingerprint: u64,
    /// Declaration text (span up to the first `{` or `;`), normalized.
    pub signature: String,
    /// Whether the item carries a `///` doc comment.
    pub has_docs: bool,
}

/// Public items keyed by full path (`crate::module::item`).
#[derive(Debug, Default)]
pub struct Surface {
    pub items: BTreeMap<String, ItemInfo>,
}

/// One distinct item, grouped by (kind, fingerprint) to collapse the extra
/// path keys `alias_reexports` adds for the same underlying function —
/// `representative` is the shortest path (for display), `aliases` holds
/// every spelling (for lookups, since a `covers=` tag may name any of them).
/// Un-fingerprinted items (span unreadable) are never merged with others.
pub struct ItemGroup<'a> {
    pub representative: &'a String,
    pub aliases: Vec<&'a String>,
}

impl Surface {
    /// Every item in this surface, deduplicated across `pub use` aliases.
    pub fn grouped(&self) -> Vec<ItemGroup<'_>> {
        let mut groups: HashMap<(&str, u64), Vec<&String>> = HashMap::new();
        let mut unfingerprinted = Vec::new();
        for (path, info) in &self.items {
            if info.fingerprint == 0 {
                unfingerprinted.push(path);
                continue;
            }
            groups
                .entry((info.kind.as_str(), info.fingerprint))
                .or_default()
                .push(path);
        }
        let mut out: Vec<ItemGroup<'_>> = groups
            .into_values()
            .map(|mut aliases| {
                aliases.sort();
                let representative = aliases.iter().min_by_key(|p| p.len()).copied().unwrap();
                ItemGroup {
                    representative,
                    aliases,
                }
            })
            .chain(unfingerprinted.into_iter().map(|p| ItemGroup {
                representative: p,
                aliases: vec![p],
            }))
            .collect();
        out.sort_by(|a, b| a.representative.cmp(b.representative));
        out
    }
}

/// Item kinds surfaced in reports (impl blocks and imports are noise).
const KINDS: &[&str] = &[
    "function",
    "struct",
    "enum",
    "trait",
    "constant",
    "type_alias",
    "module",
    "static",
    "union",
    "macro",
    "proc_macro",
];

/// Build a surface from a rustdoc JSON document. `source_root` resolves the
/// relative filenames in spans (the directory rustdoc ran in).
pub fn from_rustdoc(doc: &Value, source_root: &Path) -> Surface {
    let mut surface = Surface::default();
    let (Some(index), Some(paths)) = (doc["index"].as_object(), doc["paths"].as_object()) else {
        return surface;
    };
    let mut file_cache: HashMap<String, Vec<String>> = HashMap::new();

    for (id, item) in index {
        if item["visibility"] != "public" {
            continue;
        }
        let Some(inner) = item["inner"].as_object() else {
            continue;
        };
        let Some(kind) = inner.keys().next().map(String::as_str) else {
            continue;
        };
        if !KINDS.contains(&kind) {
            continue;
        }
        // unaddressable (e.g. non-re-exported assoc items) if absent
        let Some(full) = item_path(paths, id) else {
            continue;
        };

        let (fingerprint, signature) =
            span_fingerprint(&item["span"], source_root, &mut file_cache)
                .unwrap_or((0, String::new()));
        let has_docs = item["docs"].as_str().is_some_and(|d| !d.trim().is_empty());

        surface.items.insert(
            full,
            ItemInfo {
                kind: kind.to_string(),
                fingerprint,
                signature,
                has_docs,
            },
        );
    }

    alias_reexports(index, paths, &mut surface);
    surface
}

/// Register every `pub use inner::item;` re-export under an additional key:
/// the *importing* module's path + the local alias name.
///
/// rustdoc's `paths` map gives each item its structural definition site,
/// which for a private-submodule-plus-flattening `pub use` (e.g. `mod sma;
/// pub use sma::sma;`) runs through a module that's never itself `pub` — so
/// rustdoc reports `myc::indicators::sma::sma` even though only
/// `myc::indicators::sma` is externally valid. Bind markers, `covers = "..."`
/// tags, and doc prose all read naturally as the flattened path a consumer
/// would actually import, so both spellings are registered (see
/// `Surface::grouped` for how counting avoids double-charging the alias).
fn alias_reexports(
    index: &serde_json::Map<String, Value>,
    paths: &serde_json::Map<String, Value>,
    surface: &mut Surface,
) {
    for (mod_id, item) in index {
        if item["visibility"] != "public" {
            continue;
        }
        let Some(children) = item["inner"]["module"]["items"].as_array() else {
            continue;
        };
        let Some(mod_path) = item_path(paths, mod_id) else {
            continue;
        };

        for child in children {
            let Some(child_id) = child.as_u64() else {
                continue;
            };
            let child_id = child_id.to_string();
            let Some(use_) = index.get(&child_id).map(|c| &c["inner"]["use"]) else {
                continue;
            };
            let Some(target_id) = use_["id"].as_u64() else {
                continue;
            };
            // A named re-export (`pub use mod::item;`) aliases exactly one
            // item; a glob (`pub use mod::*;`) — the near-universal way this
            // codebase flattens a whole file-per-item submodule — aliases
            // every one of the target module's own children under its own
            // name instead.
            let targets: Vec<(String, u64)> = if use_["is_glob"].as_bool().unwrap_or(false) {
                glob_targets(index, target_id)
            } else {
                match use_["name"].as_str() {
                    Some(name) => vec![(name.to_string(), target_id)],
                    None => continue,
                }
            };

            for (name, target_id) in targets {
                alias_one(paths, surface, &mod_path, &name, target_id);
            }
        }
    }
}

/// Every direct child of a glob-imported module, as `(its own name, id)` —
/// what `pub use module::*;` actually brings into scope.
fn glob_targets(index: &serde_json::Map<String, Value>, module_id: u64) -> Vec<(String, u64)> {
    let Some(children) = index
        .get(&module_id.to_string())
        .and_then(|m| m["inner"]["module"]["items"].as_array())
    else {
        return Vec::new();
    };
    children
        .iter()
        .filter_map(|c| {
            let id = c.as_u64()?;
            let name = index.get(&id.to_string())?["name"].as_str()?;
            Some((name.to_string(), id))
        })
        .collect()
}

/// Register one alias (`{mod_path}::{name}` -> the target's info) alongside
/// its canonical entry, never replacing it — existing bind markers and
/// `covers=` tags may reference either spelling, so both must keep
/// resolving. `Surface::grouped` handles deduping for consumer-facing counts.
fn alias_one(
    paths: &serde_json::Map<String, Value>,
    surface: &mut Surface,
    mod_path: &str,
    name: &str,
    target_id: u64,
) {
    let Some(target_path) = item_path(paths, &target_id.to_string()) else {
        return;
    };
    let Some(info) = surface.items.get(&target_path).cloned() else {
        return; // target wasn't a `KINDS` item (or was private/unaddressable)
    };
    let alias = format!("{mod_path}::{name}");
    if alias == target_path {
        return;
    }
    surface.items.entry(alias).or_insert(info);
}

fn item_path(paths: &serde_json::Map<String, Value>, id: &str) -> Option<String> {
    let segs = paths.get(id)?["path"].as_array()?;
    Some(
        segs.iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("::"),
    )
}

fn span_fingerprint(
    span: &Value,
    root: &Path,
    cache: &mut HashMap<String, Vec<String>>,
) -> Option<(u64, String)> {
    let filename = span["filename"].as_str()?;
    let begin = span["begin"].as_array()?;
    let end = span["end"].as_array()?;
    let (bl, el) = (begin[0].as_u64()? as usize, end[0].as_u64()? as usize);

    let lines = match cache.get(filename) {
        Some(l) => l,
        None => {
            // Strict root-relative read: a CWD fallback would silently read
            // the wrong tree when fingerprinting another worktree's build.
            let abs = root.join(filename);
            let text = std::fs::read_to_string(&abs).ok()?;
            cache.insert(
                filename.to_string(),
                text.lines().map(str::to_string).collect(),
            );
            &cache[filename]
        }
    };
    if bl == 0 || el > lines.len() || bl > el {
        return None;
    }
    let source = lines[bl - 1..el].join("\n");
    let normalized: String = source.split_whitespace().collect::<Vec<_>>().join(" ");
    let sig_end = normalized
        .find('{')
        .or_else(|| normalized.find(';'))
        .unwrap_or(normalized.len());
    let signature = normalized[..sig_end].trim().to_string();
    Some((soothfast_registry::fnv1a(normalized.as_bytes()), signature))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A rustdoc JSON slice for `mod sma;` (private) + `pub use sma::sma;`
    /// (flattening): the canonical path runs through the private submodule.
    fn private_submodule_reexport_doc() -> Value {
        json!({
            "index": {
                "1": {
                    "visibility": "public",
                    "inner": { "module": { "items": [2, 3] } },
                },
                "2": {
                    "name": "sma",
                    "visibility": "public",
                    "inner": { "function": {} },
                    "span": { "filename": "src/sma.rs", "begin": [1, 0], "end": [1, 0] },
                    "docs": "Simple moving average.",
                },
                "3": {
                    "visibility": "public",
                    "inner": {
                        "use": { "source": "sma::sma", "name": "sma", "id": 2, "is_glob": false },
                    },
                },
            },
            "paths": {
                "1": { "path": ["myc", "indicators"], "kind": "module" },
                "2": { "path": ["myc", "indicators", "sma", "sma"], "kind": "function" },
            },
        })
    }

    #[test]
    fn reexport_alias_resolves_alongside_canonical_path() {
        let dir =
            std::env::temp_dir().join(format!("soothfast-surface-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/sma.rs"), "pub fn sma() {}\n").unwrap();

        let doc = private_submodule_reexport_doc();
        let surface = from_rustdoc(&doc, &dir);

        assert!(surface.items.contains_key("myc::indicators::sma::sma"));
        assert!(
            surface.items.contains_key("myc::indicators::sma"),
            "expected the flattened re-export path to resolve too: {:?}",
            surface.items.keys().collect::<Vec<_>>()
        );
        let canonical = &surface.items["myc::indicators::sma::sma"];
        let alias = &surface.items["myc::indicators::sma"];
        assert_eq!(canonical.fingerprint, alias.fingerprint);
        assert_eq!(alias.kind, "function");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn glob_reexport_aliases_every_child_by_its_own_name() {
        // `mod sma; pub use sma::*;`, vs. the single-item form above.
        let mut doc = private_submodule_reexport_doc();
        doc["index"]["3"]["inner"]["use"] =
            json!({ "source": "sma", "name": "sma", "id": 7, "is_glob": true });
        doc["index"]["7"] = json!({
            "visibility": "public",
            "inner": { "module": { "items": [2] } },
        });

        let dir =
            std::env::temp_dir().join(format!("soothfast-surface-glob-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/sma.rs"), "pub fn sma() {}\n").unwrap();

        let surface = from_rustdoc(&doc, &dir);
        assert!(surface.items.contains_key("myc::indicators::sma::sma"));
        assert!(surface.items.contains_key("myc::indicators::sma"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn item(kind: &str, fingerprint: u64) -> ItemInfo {
        ItemInfo {
            kind: kind.to_string(),
            fingerprint,
            signature: String::new(),
            has_docs: false,
        }
    }

    fn surface_of(items: Vec<(&str, ItemInfo)>) -> Surface {
        Surface {
            items: items.into_iter().map(|(p, i)| (p.to_string(), i)).collect(),
        }
    }

    #[test]
    fn three_aliases_of_one_function_collapse_to_one_group() {
        let surf = surface_of(vec![
            ("myc::atr", item("function", 42)),
            ("myc::indicators::atr", item("function", 42)),
            ("myc::indicators::atr::atr", item("function", 42)),
        ]);
        let groups = surf.grouped();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].representative, "myc::atr");
        assert_eq!(groups[0].aliases.len(), 3);
    }

    #[test]
    fn covers_tag_on_a_longer_alias_still_counts_as_measured() {
        // A `covers=` tag might name the module-level alias even though the
        // crate-root re-export is shorter — checking "is this covered" must
        // consult every alias, not just the (display-only) shortest one.
        let surf = surface_of(vec![
            ("myc::atr", item("function", 42)),
            ("myc::indicators::atr", item("function", 42)),
        ]);
        let g = &surf.grouped()[0];
        assert!(
            g.aliases
                .iter()
                .any(|a| a.as_str() == "myc::indicators::atr")
        );
    }

    #[test]
    fn distinct_functions_with_no_fingerprint_are_never_merged() {
        let surf = surface_of(vec![
            ("myc::a", item("function", 0)),
            ("myc::b", item("function", 0)),
        ]);
        assert_eq!(surf.grouped().len(), 2);
    }

    #[test]
    fn different_kinds_with_the_same_fingerprint_are_not_merged() {
        let surf = surface_of(vec![
            ("myc::a", item("function", 7)),
            ("myc::A", item("struct", 7)),
        ]);
        assert_eq!(surf.grouped().len(), 2);
    }
}
