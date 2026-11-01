//! The registering package of each item a save writes. However a run was
//! assembled (measured, read from the run cache, or spliced from the item
//! cache), each item's `pkg` comes from the `--list --json` listing of the
//! binary that ran, which describes exactly that binary, and never from a
//! stored or spliced item; with no listing it is unknown.
//! [`crate::ambiguity`] then decides which package owns an id two list.

use std::collections::BTreeMap;
use std::path::Path;

use crate::invoke::{self, CommonArgs, Listed, Run};

/// `run` with each item's `pkg` taken from `listing`, `None` for an item the
/// listing does not name or names no package for, whatever pkg the run held.
pub fn stamped(run: &Run, listing: &[Listed]) -> Run {
    let pkgs: BTreeMap<&str, &String> = (listing.iter())
        .filter_map(|(id, pkg)| Some((id.as_str(), pkg.as_ref()?)))
        .collect();
    let mut out = run.clone();
    for (id, m) in &mut out.items {
        m.pkg = pkgs.get(id.as_str()).map(|pkg| (*pkg).clone());
    }
    out.listed = true;
    out
}

/// `run` for a save with no listing: every item's `pkg` unknown.
pub fn unlisted(run: &Run) -> Run {
    let mut out = run.clone();
    for m in out.items.values_mut() {
        m.pkg = None;
    }
    out.listed = false;
    out
}

/// `run` as a save writes it, stamped from the listing of `exe`, the bench
/// binary that ran.
pub fn for_save(run: &Run, common: &CommonArgs, exe: &Path) -> Run {
    match invoke::listing(exe, common.filter.as_deref()) {
        Some(listing) => stamped(run, &listing),
        None => unlisted(run),
    }
}

#[cfg(test)]
mod tests;
