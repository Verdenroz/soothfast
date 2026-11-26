//! Shared surface plumbing for docs / report / mcp commands.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;
use soothfast_docs::{diff, surface};
use soothfast_report::surface::SurfaceEntry;

use crate::{invoke, refrun, refsurface, workspace};

/// Docs text per item path, keyed like [`surface::Surface::items`].
pub type DocsText = BTreeMap<String, String>;

/// Surface + per-item docs text for one package (nightly rustdoc).
pub fn load(pkg: &str, features: Option<&str>) -> Result<(surface::Surface, DocsText), String> {
    let (surface, docs, _) = surface_in(pkg, None, features)?;
    Ok((surface, docs))
}

/// The public surface of `pkg` in the tree at `dir` (the workspace when
/// `None`), its items' docs text and its source root. What it re-exports
/// from other workspace members resolves in their rustdoc JSON, built with
/// the features `pkg`'s own build enables in them.
pub fn surface_in(
    pkg: &str,
    dir: Option<&Path>,
    features: Option<&str>,
) -> Result<(surface::Surface, DocsText, PathBuf), String> {
    let (doc, root) = invoke::rustdoc_json_in(pkg, dir, features, invoke::Visibility::Public)
        .map_err(|e| e.to_string())?;
    let reexported = surface::reexported_crates(&doc);
    let dependencies = if reexported.is_empty() {
        Dependencies::default()
    } else {
        Members::of(pkg, dir, features, &reexported)?.build_all(&reexported)?
    };
    let surface = surface::from_rustdoc_with(&doc, &root, &dependencies.surfaces);
    let docs = dependencies.docs_for(&surface, docs_text(&doc));
    Ok((surface, docs, root))
}

/// The surfaces of the workspace members a package re-exports from, and
/// their items' docs text, by lib crate name.
#[derive(Default)]
struct Dependencies {
    surfaces: BTreeMap<String, surface::Surface>,
    docs: BTreeMap<String, DocsText>,
}

impl Dependencies {
    /// `own`, plus the docs of each item of `surface` a dependency defines.
    /// The item is found by kind and fingerprint, which the doc comment is
    /// part of, so a match carries the same docs.
    fn docs_for(&self, surface: &surface::Surface, own: DocsText) -> DocsText {
        let defined: HashMap<(&str, u64), &String> = (self.surfaces.iter())
            .flat_map(|(name, dep)| {
                let docs = &self.docs[name];
                dep.items.iter().filter_map(move |(path, info)| {
                    Some(((info.kind.as_str(), info.fingerprint), docs.get(path)?))
                })
            })
            .filter(|((_, fingerprint), _)| *fingerprint != 0)
            .collect();
        let mut docs = own;
        for (path, info) in &surface.items {
            if let Some(text) = defined.get(&(info.kind.as_str(), info.fingerprint)) {
                docs.entry(path.clone()).or_insert_with(|| (*text).clone());
            }
        }
        docs
    }
}

/// The workspace members a package depends on, by lib crate name, with the
/// features its build enables in each.
struct Members<'a> {
    dir: Option<&'a Path>,
    packages: BTreeMap<String, String>,
    features: BTreeMap<String, String>,
}

impl<'a> Members<'a> {
    /// No feature resolution when none of `reexported` is a member: a
    /// `pub use std::...` alone costs one `cargo metadata`.
    fn of(
        pkg: &str,
        dir: Option<&'a Path>,
        features: Option<&str>,
        reexported: &BTreeSet<String>,
    ) -> Result<Self, String> {
        let packages: BTreeMap<String, String> = workspace::local_deps(pkg, dir)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|name| (name.replace('-', "_"), name))
            .collect();
        let features = if reexported.iter().any(|c| packages.contains_key(c)) {
            invoke::dependency_features(pkg, dir, features).map_err(|e| e.to_string())?
        } else {
            BTreeMap::new()
        };
        Ok(Members {
            dir,
            packages,
            features,
        })
    }

    /// Every member among `crates`, and the members those re-export from in
    /// turn.
    fn build_all(&self, crates: &BTreeSet<String>) -> Result<Dependencies, String> {
        let mut built = Dependencies::default();
        for name in crates {
            self.build(name, &mut built)?;
        }
        Ok(built)
    }

    fn build(&self, name: &str, built: &mut Dependencies) -> Result<(), String> {
        let Some(pkg) = self.packages.get(name) else {
            return Ok(());
        };
        if built.surfaces.contains_key(name) {
            return Ok(());
        }
        let features = self.features.get(name).map_or("", String::as_str);
        let (doc, root) =
            invoke::rustdoc_json_exactly(pkg, self.dir, features, invoke::Visibility::Public)
                .map_err(|e| e.to_string())?;
        for dep in surface::reexported_crates(&doc) {
            self.build(&dep, built)?;
        }
        let surface = surface::from_rustdoc_with(&doc, &root, &built.surfaces);
        let docs = built.docs_for(&surface, docs_text(&doc));
        built.surfaces.insert(name.to_string(), surface);
        built.docs.insert(name.to_string(), docs);
        Ok(())
    }
}

/// Docs text per item path from a rustdoc JSON document.
pub fn docs_text(doc: &Value) -> DocsText {
    let mut out = DocsText::new();
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
    let (surf, docs) = load(pkg, features)?;
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
/// When HEAD is the merge-base there is no diff and nothing is built.
/// Otherwise the merge-base's surface is read from the run store before it is
/// built in a worktree, since the surface of a commit never changes.
pub fn surface_diff_text(
    pkg: &str,
    refname: &str,
    features: Option<&str>,
) -> Result<String, String> {
    let root = invoke::workspace_root().map_err(|e| e.to_string())?;
    let anchor = refrun::anchor(&root, refname);
    if anchor == refrun::Anchor::Head {
        println!("report: no commits since {refname}, so {pkg} has no API diff");
    }
    surface_diff(
        anchor,
        || load(pkg, features).map(|(surface, _)| surface),
        |sha| merge_base_surface(&root, sha, pkg, refname, features),
    )
}

/// The public surface of `pkg` at `sha`, the merge-base with `refname`: the
/// stored one, else one built in a worktree and then kept.
fn merge_base_surface(
    root: &Path,
    sha: &str,
    pkg: &str,
    refname: &str,
    features: Option<&str>,
) -> Result<surface::Surface, String> {
    let visibility = invoke::Visibility::Public;
    let key = invoke::rustdoc_build(root)
        .filter(|_| !sha.is_empty())
        .map(|build| refsurface::key(sha, pkg, features, visibility, &build));
    if let Some(surface) = refsurface::stored(key.as_deref()) {
        println!("report: reusing the stored API surface of {refname} for {pkg}");
        return Ok(surface);
    }
    println!("report: building the API surface of {refname} in a worktree for {pkg}");
    let surface = invoke::with_merge_base_worktree(refname, |wt| {
        surface_in(pkg, Some(wt), features)
            .map(|(surface, _, _)| surface)
            .map_err(|e| format!("cannot build surface at the merge-base: {e}"))
    })?;
    refsurface::keep(key.as_deref(), &surface);
    Ok(surface)
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
mod reexport_tests;

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
                spans: Vec::new(),
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
