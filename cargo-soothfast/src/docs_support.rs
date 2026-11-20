//! Shared surface plumbing for docs / report / mcp commands.

use serde_json::Value;
use soothfast_docs::{diff, surface};
use soothfast_report::surface::SurfaceEntry;

use crate::{invoke, refrun, refsurface};

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

/// Surface entries: every public item with signature + full doc comment.
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
///
/// A ref that is HEAD has no diff and builds nothing. Otherwise the
/// merge-base's surface is read from the run store before it is built in a
/// worktree, since the surface of a commit never changes.
pub fn surface_diff_text(
    pkg: &str,
    refname: &str,
    features: Option<&str>,
) -> Result<String, String> {
    let root = invoke::workspace_root().map_err(|e| e.to_string())?;
    let anchor = refrun::anchor(&root, refname);
    if anchor == refrun::Anchor::Head {
        println!("report: {refname} is HEAD, so {pkg} has no API diff");
    }
    surface_diff(
        anchor,
        || load(pkg, features).map(|(surface, _)| surface),
        |sha| {
            let visibility = invoke::Visibility::Public;
            let key = invoke::rustdoc_rustc(&root)
                .filter(|_| !sha.is_empty())
                .map(|rustc| refsurface::key(sha, pkg, features, visibility, &rustc));
            let (surface, stored) = refsurface::reuse(key.as_deref(), || {
                println!("report: building the API surface of {refname} in a worktree for {pkg}");
                invoke::with_merge_base_worktree(refname, |wt| {
                    invoke::rustdoc_json_in(pkg, Some(wt), features, visibility)
                        .map(|(doc, wt_root)| surface::from_rustdoc(&doc, &wt_root))
                        .map_err(|e| format!("cannot build surface at the merge-base: {e}"))
                })
            })?;
            if stored {
                println!("report: reusing the stored API surface of {refname} for {pkg}");
            }
            Ok(surface)
        },
    )
}

fn surface_diff(
    anchor: refrun::Anchor,
    head: impl FnOnce() -> Result<surface::Surface, String>,
    merge_base: impl FnOnce(&str) -> Result<surface::Surface, String>,
) -> Result<String, String> {
    let refrun::Anchor::At(sha) = anchor else {
        return Ok(String::new());
    };
    let new_surf = head()?;
    let old_surf = merge_base(&sha)?;
    let d = diff::compare(&old_surf, &new_surf);
    Ok(if d.is_empty() {
        String::new()
    } else {
        diff::render(&d)
    })
}

#[cfg(test)]
mod tests {
    use soothfast_docs::surface::{ItemInfo, Surface};

    use super::{refrun, surface_diff};

    fn surface(paths: &[&str]) -> Surface {
        let mut s = Surface::default();
        for path in paths {
            let info = ItemInfo {
                kind: "function".into(),
                fingerprint: 1,
                signature: format!("pub fn {path}()"),
                has_docs: true,
            };
            s.items.insert(path.to_string(), info);
        }
        s
    }

    #[test]
    fn a_ref_that_is_head_builds_no_surface() {
        let never = || -> Result<Surface, String> { panic!("built a surface") };
        let text = surface_diff(refrun::Anchor::Head, never, |_| never());
        assert_eq!(text, Ok(String::new()));
    }

    #[test]
    fn an_earlier_commit_is_diffed_against_its_own_surface() {
        let text = surface_diff(
            refrun::Anchor::At("abc".into()),
            || Ok(surface(&["demo::a", "demo::b"])),
            |sha| {
                assert_eq!(sha, "abc");
                Ok(surface(&["demo::a"]))
            },
        )
        .unwrap();
        assert!(text.contains("demo::b"), "{text}");
    }
}
