//! Copying the entries a run built or found into the directory a job uploads.

use std::path::{Path, PathBuf};

use super::live::Live;
use super::select::Item;
use crate::invoke::{self, Build};
use crate::{benchexe, binstore, runcache};

/// Copy the entry `entry` of the store in `store` into `stage`, with the
/// manifest and group of its `key` and the files kept beside it.
pub(super) fn stage_entry(store: &Path, entry: &Path, key: &str, stage: &Path) {
    let (Some(name), Some(entry_name)) = (store.file_name(), entry.file_name()) else {
        return;
    };
    let to = stage.join(name);
    let _ = std::fs::create_dir_all(&to);
    let beside = entry_name.to_string_lossy().into_owned();
    for file in std::fs::read_dir(store).into_iter().flatten().flatten() {
        let file_name = file.file_name();
        let wanted = file_name == entry_name
            || file_name
                .to_string_lossy()
                .starts_with(&format!("{beside}."))
            || file_name == format!("{key}.manifest").as_str()
            || file_name == format!("{key}.group").as_str();
        if wanted {
            let _ = std::fs::copy(file.path(), to.join(file_name));
        }
    }
}

/// The key of the entry holding `exe`, which is named `<key>-<digest>.bin`.
pub(super) fn executable_entry(exe: &Path) -> Option<(PathBuf, String)> {
    let entry = binstore::entry_of(exe)?;
    let key = entry.file_name()?.to_str()?.split_once('-')?.0.to_string();
    Some((entry, key))
}

/// Copy what a later job reads for `item` into `stage`: its entry, the
/// manifest naming its inputs, and the digest and extents kept beside it.
pub(super) fn stage_item(item: &Item, built: Option<PathBuf>, live: &Live, stage: &Path) {
    let from_exe = |exe: Option<PathBuf>| {
        if let (Some((entry, key)), Some(store)) =
            (exe.as_deref().and_then(executable_entry), binstore::dir())
        {
            stage_entry(&store, &entry, &key, stage);
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
                stage_entry(&store, &entry, &key, stage);
            }
        }
    }
}
