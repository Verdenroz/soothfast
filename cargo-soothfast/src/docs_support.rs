//! Shared surface plumbing for docs / report / mcp commands.

use serde_json::Value;
use soothfast_docs::{diff, surface};
use soothfast_report::llms::SurfaceEntry;

use crate::invoke;

/// Surface + per-item docs text for one package (nightly rustdoc).
pub fn load(pkg: &str, features: Option<&str>) -> Result<(surface::Surface, Value), String> {
    let (doc, root) = invoke::rustdoc_json_in(pkg, None, features, invoke::Visibility::Public)
        .map_err(|e| e.to_string())?;
    Ok((surface::from_rustdoc(&doc, &root), doc))
}

/// Docs text per item path from a rustdoc JSON document.
pub fn docs_text(doc: &Value) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let (Some(index), Some(paths)) = (doc["index"].as_object(), doc["paths"].as_object()) else {
        return out;
    };
    for (id, item) in index {
        if let (Some(docs), Some(p)) = (item["docs"].as_str(), paths.get(id))
            && let Some(segs) = p["path"].as_array()
        {
            let full: Vec<&str> = segs.iter().filter_map(Value::as_str).collect();
            out.insert(
                full.join("::"),
                soothfast_docs::reference::resolve_reference_links(docs),
            );
        }
    }
    out
}

/// llms.txt entries: every public item with signature + full doc comment.
pub fn surface_entries(pkg: &str, features: Option<&str>) -> Result<Vec<SurfaceEntry>, String> {
    let (surf, doc) = load(pkg, features)?;
    let docs = docs_text(&doc);
    Ok(surf
        .items
        .iter()
        .map(|(path, info)| {
            let full_docs = docs.get(path).cloned().unwrap_or_default();
            let summary = full_docs.lines().next().unwrap_or("").to_string();
            SurfaceEntry {
                path: path.clone(),
                kind: info.kind.clone(),
                signature: info.signature.clone(),
                pkg: pkg.to_string(),
                summary,
                docs: full_docs,
            }
        })
        .collect())
}

/// Rendered public-surface diff of `pkg` HEAD vs the merge-base with `refname`.
pub fn surface_diff_text(
    pkg: &str,
    refname: &str,
    features: Option<&str>,
) -> Result<String, String> {
    let (new_surf, _) = load(pkg, features)?;

    let old_surf = invoke::with_merge_base_worktree(refname, |wt| {
        invoke::rustdoc_json_in(pkg, Some(wt), features, invoke::Visibility::Public)
            .map(|(doc, wt_root)| surface::from_rustdoc(&doc, &wt_root))
            .map_err(|e| format!("cannot build surface at the merge-base: {e}"))
    })?;

    let d = diff::compare(&old_surf, &new_surf);
    Ok(if d.is_empty() {
        String::new()
    } else {
        diff::render(&d)
    })
}
