//! Public-surface diff between two builds: what a reviewer needs to see
//! about API-relevant change, and the raw material for changelog drafting.

use crate::surface::Surface;

/// Public-API delta between two [`Surface`] snapshots, by item path.
#[derive(Debug, Default)]
pub struct SurfaceDiff {
    /// Item paths present only in the new surface.
    pub added: Vec<String>,
    /// Item paths present only in the old surface.
    pub removed: Vec<String>,
    /// (path, signature_changed) — false means body-only change.
    pub changed: Vec<(String, bool)>,
}

impl SurfaceDiff {
    /// True when the two surfaces are identical: nothing added, removed,
    /// or changed.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Diff two surfaces: added/removed items by path, changed items by span
/// fingerprint (modules are exempt — their span is the whole file).
pub fn compare(old: &Surface, new: &Surface) -> SurfaceDiff {
    let mut diff = SurfaceDiff::default();
    for (path, info) in &new.items {
        match old.items.get(path) {
            None => diff.added.push(path.clone()),
            Some(prev) => {
                // A module's span is its whole file — any edit anywhere would
                // flag it. Added/removed modules still report.
                if info.kind != "module" && prev.fingerprint != info.fingerprint {
                    diff.changed
                        .push((path.clone(), prev.signature != info.signature));
                }
            }
        }
    }
    for path in old.items.keys() {
        if !new.items.contains_key(path) {
            diff.removed.push(path.clone());
        }
    }
    diff
}

/// One `ADDED`/`REMOVED`/`CHANGED` line per item, ready for terminals and
/// changelog drafts; an empty diff renders as a single "no changes" line.
pub fn render(diff: &SurfaceDiff) -> String {
    if diff.is_empty() {
        return "surface: no public API changes\n".into();
    }
    let mut out = String::new();
    for p in &diff.added {
        out.push_str(&format!("ADDED    {p}\n"));
    }
    for p in &diff.removed {
        out.push_str(&format!("REMOVED  {p}\n"));
    }
    for (p, sig) in &diff.changed {
        let what = if *sig { "signature" } else { "body" };
        out.push_str(&format!("CHANGED  {p} ({what})\n"));
    }
    out
}
