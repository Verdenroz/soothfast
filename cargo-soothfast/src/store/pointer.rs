//! The pointer from a key's stable half to the last entry stored under it:
//! `<stable>.latest` names a cheap key, and `<cheap>.stable` names the stable
//! half back, with the time the entry was built.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::write_atomic;

/// Whether `text` is one half of a key, so a file named for it lies in the
/// store directory.
fn is_half(text: &str) -> bool {
    text.len() == 16 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn read(dir: &Path, name: String) -> Option<String> {
    std::fs::read_to_string(dir.join(name)).ok()
}

/// The cheap key `stable` points at, whatever its entry holds.
fn target(dir: &Path, stable: &str) -> Option<String> {
    read(dir, format!("{stable}.latest")).filter(|cheap| is_half(cheap))
}

/// When the entry named for `cheap` was built, if it belongs to `stable`. A
/// pointer left by a merge or a torn write can name another build's entry.
pub(super) fn born(dir: &Path, cheap: &str, stable: &str) -> Option<u128> {
    let text = read(dir, format!("{cheap}.stable"))?;
    let (owner, born) = text.split_once('\n')?;
    (owner == stable).then(|| born.parse().ok()).flatten()
}

/// The cheap key of the last entry stored under `stable`, and when it was
/// built.
pub(super) fn latest(dir: &Path, stable: &str) -> Option<(String, u128)> {
    let cheap = target(dir, stable)?;
    let born = born(dir, &cheap, stable)?;
    Some((cheap, born))
}

/// Nanoseconds since the epoch: an age that is part of the file, so an
/// archive, a cache restore and a copy all keep it.
pub(super) fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// Record `cheap`, built at `born`, as the latest entry of `stable`.
pub(super) fn remember(dir: &Path, cheap: &str, stable: &str, born: u128) {
    let back = format!("{stable}\n{born}");
    write_atomic(&dir.join(format!("{cheap}.stable")), back.as_bytes());
    write_atomic(&dir.join(format!("{stable}.latest")), cheap.as_bytes());
}

/// The name of the pointer that finds the entry named for `cheap`.
pub fn name_of(dir: &Path, cheap: &str) -> Option<String> {
    let text = read(dir, format!("{cheap}.stable"))?;
    let (stable, _) = text.split_once('\n')?;
    Some(format!("{stable}.latest"))
}

/// Give `into` each pointer of `from` whose entry is newer than the one
/// `into` points at, or whose stable half `into` has no usable pointer for.
/// Run it after the entries of `from` are copied and the store is pruned: a
/// pointer whose entry is not in `into` loses.
pub fn merge(from: &Path, into: &Path) {
    let Ok(files) = std::fs::read_dir(from) else {
        return;
    };
    let payloads: Vec<String> = std::fs::read_dir(into)
        .into_iter()
        .flatten()
        .flatten()
        .map(|f| f.file_name().to_string_lossy().into_owned())
        .filter(|name| super::is_payload(name))
        .collect();
    let built = |cheap: &str, stable: &str| {
        let prefix = format!("{cheap}-");
        let born = born(into, cheap, stable)?;
        payloads
            .iter()
            .any(|p| p.starts_with(&prefix))
            .then_some(born)
    };
    for file in files.flatten() {
        let name = file.file_name().to_string_lossy().into_owned();
        let Some(stable) = name.strip_suffix(".latest").filter(|s| is_half(s)) else {
            continue;
        };
        let Some(theirs) = target(from, stable) else {
            continue;
        };
        let Some(their_born) = built(&theirs, stable) else {
            continue;
        };
        let ours = target(into, stable).and_then(|cheap| built(&cheap, stable));
        if ours.is_none_or(|born| born < their_born) {
            write_atomic(&into.join(&name), theirs.as_bytes());
        }
    }
}
