//! Copying the entries a run built or found into the directory a job uploads.

use std::path::{Path, PathBuf};

use super::live::Live;
use super::select::Item;
use crate::invoke::{self, Build};
use crate::{benchexe, binstore, runcache};

/// Copy `entry` and what belongs to it into `stage`, under the name of the
/// store directory it lies in.
pub(super) fn stage_entry(entry: &Path, stage: &Path) {
    let Some(to) = entry
        .parent()
        .and_then(Path::file_name)
        .map(|n| stage.join(n))
    else {
        return;
    };
    let _ = std::fs::create_dir_all(&to);
    for file in binstore::entry_files(entry) {
        if let Some(name) = file.file_name() {
            let _ = std::fs::copy(&file, to.join(name));
        }
    }
}

/// Copy what a later job reads for `item` into `stage`: its entry, the
/// manifest naming its inputs, and the digest and extents kept beside it.
pub(super) fn stage_item(item: &Item, built: Option<PathBuf>, live: &Live, stage: &Path) {
    let from_exe = |exe: Option<PathBuf>| {
        if let Some(entry) = exe.as_deref().and_then(binstore::entry_of) {
            stage_entry(&entry, stage);
        }
    };
    match item {
        Item::Bench { .. } => from_exe(built),
        Item::Routes { pkg, target } => from_exe(built.or_else(|| {
            benchexe::executable(&live.one(pkg, Some(target)), None, None, Build::Discover)
        })),
        Item::Rustdoc { .. } => {
            let (Some(key), Some(store), Ok(root)) = (
                live.rustdoc_key(item),
                runcache::state_dir("rustdoc"),
                invoke::workspace_root(),
            ) else {
                return;
            };
            if let Some(entry) = crate::store::lookup(&store, &key, &root, "json") {
                stage_entry(&entry, stage);
            }
        }
    }
}
