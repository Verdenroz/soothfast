//! Public API surface from rustdoc JSON: item paths, span-derived
//! fingerprints, doc presence, and literal source signatures.
//!
//! Fingerprints hash the item's *source span text* (ordinary comments
//! stripped, then whitespace-normalized), not rustdoc's structured types —
//! rustdoc item ids shift across builds and would churn a structure-based
//! hash.

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
    /// FNV-1a of the span source, comments stripped then whitespace-normalized.
    pub fingerprint: u64,
    /// Declaration text, normalized: the span up to the first `{` or `;`,
    /// except that a struct or union keeps its `pub` fields and an enum its
    /// variants, since those are what a consumer writes against.
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
            span_fingerprint(&item["span"], kind, source_root, &mut file_cache)
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

/// Kinds whose braces enclose part of the public contract rather than an
/// implementation: every enum variant, and a struct's or union's `pub`
/// fields.
const MEMBER_KINDS: [&str; 3] = ["struct", "enum", "union"];

fn span_fingerprint(
    span: &Value,
    kind: &str,
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
    let span_text = lines[bl - 1..el].join("\n");
    let normalized = normalize(&crate::comments::strip(&span_text));
    // Doc comments are part of the fingerprint (prose is bound to them)
    // but not of the signature, where they would hide a member's `pub`.
    let bare = normalize(&crate::comments::strip_all(&span_text));
    let sig_end = bare
        .find('{')
        .or_else(|| bare.find(';'))
        .unwrap_or(bare.len());
    let declaration = bare[..sig_end].trim();
    let signature = if MEMBER_KINDS.contains(&kind) {
        member_signature(declaration, &bare[sig_end..], kind == "enum")
    } else {
        declaration.to_string()
    };
    Some((soothfast_registry::fnv1a(normalized.as_bytes()), signature))
}

fn normalize(source: &str) -> String {
    source.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Declaration plus the members a consumer can name: every variant of an
/// enum, only the `pub` fields of a struct or union. A private field edit is
/// a body change, a public one is not.
fn member_signature(declaration: &str, rest: &str, all_public: bool) -> String {
    let Some(body) = rest
        .strip_prefix('{')
        .and_then(|inner| inner.trim_end().strip_suffix('}'))
    else {
        return declaration.to_string();
    };
    let members: Vec<String> = split_top_level(body)
        .into_iter()
        .map(|member| strip_attributes(member.trim()).to_string())
        .filter(|member| !member.is_empty() && (all_public || member.starts_with("pub ")))
        .collect();
    format!("{declaration} {{ {} }}", members.join(", "))
}

/// Split on commas outside brackets, so `HashMap<K, V>` stays one member.
/// The `>` of a `->` return arrow is not a bracket.
fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let mut prev = ' ';
    for (i, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' if prev == '-' => {}
            '>' | ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        prev = c;
    }
    parts.push(&text[start..]);
    parts
}

/// Drop leading `#[...]` attributes so `#[serde(rename = "x")] pub a: u8`
/// is recognised as public.
fn strip_attributes(member: &str) -> &str {
    let mut rest = member;
    while let Some(after) = rest.strip_prefix("#[") {
        let mut depth = 1;
        let mut end = None;
        for (i, c) in after.char_indices() {
            depth += match c {
                '[' => 1,
                ']' => -1,
                _ => 0,
            };
            if depth == 0 {
                end = Some(i + 1);
                break;
            }
        }
        match end {
            Some(e) => rest = after[e..].trim_start(),
            None => break,
        }
    }
    rest
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

    fn fingerprint_of(tag: &str, source: &str) -> (u64, String) {
        fingerprint_of_kind(tag, "struct", source)
    }

    fn fingerprint_of_kind(tag: &str, kind: &str, source: &str) -> (u64, String) {
        let dir = std::env::temp_dir().join(format!(
            "soothfast-fingerprint-{}-{tag}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.rs"), source).unwrap();
        let span = json!({
            "filename": "lib.rs",
            "begin": [1, 0],
            "end": [source.lines().count(), 0],
        });
        let got = span_fingerprint(&span, kind, &dir, &mut HashMap::new()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        got
    }

    #[test]
    fn a_comment_only_edit_does_not_move_the_fingerprint() {
        let plain = fingerprint_of("plain", "pub struct S {\n    a: u8,\n}\n");
        let noted = fingerprint_of(
            "noted",
            "pub struct S {\n    // widened for the wire format\n    a: u8,\n}\n",
        );
        assert_eq!(plain.0, noted.0);
    }

    #[test]
    fn a_field_change_still_moves_the_fingerprint() {
        let before = fingerprint_of("before", "pub struct S {\n    a: u8,\n}\n");
        let after = fingerprint_of("after", "pub struct S {\n    a: u16,\n}\n");
        assert_ne!(before.0, after.0);
    }

    #[test]
    fn a_struct_field_addition_changes_the_signature() {
        let before = fingerprint_of_kind(
            "sig-before",
            "struct",
            "pub struct S {\n    pub a: u8,\n}\n",
        );
        let after = fingerprint_of_kind(
            "sig-after",
            "struct",
            "pub struct S {\n    pub a: u8,\n    pub b: u8,\n}\n",
        );
        assert_ne!(before.1, after.1);
        assert_eq!(after.1, "pub struct S { pub a: u8, pub b: u8 }");
    }

    #[test]
    fn a_private_field_edit_is_body_only() {
        let before = fingerprint_of_kind(
            "priv-before",
            "struct",
            "pub struct S {\n    pub a: u8,\n    b: u8,\n}\n",
        );
        let after = fingerprint_of_kind(
            "priv-after",
            "struct",
            "pub struct S {\n    pub a: u8,\n    b: u16,\n    c: u8,\n}\n",
        );
        assert_ne!(before.0, after.0);
        assert_eq!(before.1, after.1);
        assert_eq!(before.1, "pub struct S { pub a: u8 }");
    }

    #[test]
    fn generic_fields_and_attributes_are_read_as_members() {
        let got = fingerprint_of_kind(
            "generic",
            "struct",
            "pub struct S {\n    #[serde(rename = \"m\")]\n    pub map: HashMap<String, Vec<u8>>,\n    pub(crate) hidden: u8,\n}\n",
        );
        assert_eq!(got.1, "pub struct S { pub map: HashMap<String, Vec<u8>> }");
    }

    #[test]
    fn a_documented_public_field_counts_as_signature() {
        let before = fingerprint_of_kind(
            "pubdoc-before",
            "struct",
            "pub struct S {\n    /// count\n    pub a: u8,\n}\n",
        );
        let after = fingerprint_of_kind(
            "pubdoc-after",
            "struct",
            "pub struct S {\n    /// count\n    pub a: u8,\n    /// total\n    pub b: u8,\n}\n",
        );
        assert_ne!(before.1, after.1);
        assert_eq!(after.1, "pub struct S { pub a: u8, pub b: u8 }");
    }

    #[test]
    fn a_variant_doc_edit_moves_the_fingerprint_but_not_the_signature() {
        let before = fingerprint_of_kind(
            "vdoc-before",
            "enum",
            "pub enum E {\n    /// first\n    A,\n    B,\n}\n",
        );
        let after = fingerprint_of_kind(
            "vdoc-after",
            "enum",
            "pub enum E {\n    /// the first\n    A,\n    B,\n}\n",
        );
        assert_ne!(before.0, after.0);
        assert_eq!(before.1, after.1);
        assert_eq!(before.1, "pub enum E { A, B }");
    }

    #[test]
    fn a_function_pointer_field_does_not_swallow_its_neighbours() {
        let got = fingerprint_of_kind(
            "fnptr",
            "struct",
            "pub struct S {\n    pub f: fn(u8) -> u8,\n    pub g: u8,\n}\n",
        );
        assert_eq!(got.1, "pub struct S { pub f: fn(u8) -> u8, pub g: u8 }");
    }

    #[test]
    fn an_enum_variant_addition_changes_the_signature() {
        let before = fingerprint_of_kind("var-before", "enum", "pub enum E {\n    A,\n}\n");
        let after = fingerprint_of_kind("var-after", "enum", "pub enum E {\n    A,\n    B,\n}\n");
        assert_ne!(before.1, after.1);
    }

    #[test]
    fn a_function_body_edit_keeps_its_signature() {
        let before = fingerprint_of_kind("fn-before", "function", "pub fn f() -> u8 {\n    1\n}\n");
        let after = fingerprint_of_kind("fn-after", "function", "pub fn f() -> u8 {\n    2\n}\n");
        assert_ne!(before.0, after.0);
        assert_eq!(before.1, after.1);
        assert_eq!(before.1, "pub fn f() -> u8");
    }

    #[test]
    fn a_doc_comment_on_a_field_is_part_of_the_fingerprint() {
        let before = fingerprint_of(
            "doc-before",
            "pub struct S {\n    /// count\n    a: u8,\n}\n",
        );
        let after = fingerprint_of(
            "doc-after",
            "pub struct S {\n    /// total\n    a: u8,\n}\n",
        );
        assert_ne!(before.0, after.0);
    }
}
