//! The triage note for a gate whose build graph differs between base and head
//! under the same `--features`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

use crate::invoke::{self, CommonArgs};

const TAIL: &str = "codegen of unchanged code can shift under LTO";

/// The `[features]` table of a manifest, each feature with what it enables.
fn feature_table(manifest: &str) -> BTreeMap<String, Vec<String>> {
    let mut table = BTreeMap::new();
    let mut inside = false;
    for (_, line) in logical_lines(manifest) {
        if line.starts_with('[') {
            inside = line == "[features]";
        } else if inside
            && let Some((name, value)) = line.split_once('=')
            && let Ok(TomlValue::StrArray(items)) = parse_value(value.trim())
        {
            table.insert(name.trim().to_string(), items);
        }
    }
    table
}

/// Every feature and optional dependency `requested` turns on in `table`,
/// with `default` always on as it is for the gate's builds.
fn expand(table: &BTreeMap<String, Vec<String>>, requested: Option<&str>) -> BTreeSet<String> {
    let mut pending: Vec<String> = requested
        .into_iter()
        .flat_map(|f| f.split([',', ' ']))
        .filter(|n| !n.is_empty())
        .map(String::from)
        .chain(["default".to_string()])
        .collect();
    let mut on = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if !on.insert(name.clone()) {
            continue;
        }
        for item in table.get(&name).into_iter().flatten() {
            let target = item.strip_prefix("dep:").unwrap_or(item);
            let package = target.split_once('/').map_or(target, |(p, _)| p);
            pending.push(package.trim_end_matches('?').to_string());
        }
    }
    on.remove("default");
    on
}

/// The `name@version` of every registry or git package a `Cargo.lock` lists.
fn locked(lock: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let (mut name, mut version, mut sourced) = (None, None, false);
    let mut flush =
        |name: &mut Option<String>, version: &mut Option<String>, sourced: &mut bool| {
            if let (Some(n), Some(v)) = (name.take(), version.take())
                && *sourced
            {
                out.insert(format!("{n}@{v}"));
            }
            *sourced = false;
        };
    for line in lock.lines() {
        if line == "[[package]]" {
            flush(&mut name, &mut version, &mut sourced);
        } else if let Some(v) = line.strip_prefix("name = ") {
            name = Some(v.trim_matches('"').to_string());
        } else if let Some(v) = line.strip_prefix("version = ") {
            version = Some(v.trim_matches('"').to_string());
        } else if line.starts_with("source = ") {
            sourced = true;
        }
    }
    flush(&mut name, &mut version, &mut sourced);
    out
}

/// What differs between two sides' builds, as a leading triage line, or
/// `None` when the feature expansion and the locked dependencies agree.
pub(super) fn shift_note(
    features: Option<&str>,
    base: (&str, &str),
    head: (&str, &str),
) -> Option<String> {
    let before = expand(&feature_table(base.0), features);
    let after = expand(&feature_table(head.0), features);
    let mut changes = Vec::new();
    let label = features.filter(|f| !f.is_empty()).unwrap_or("default");
    let list = |set: Vec<&String>| {
        set.iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let added: Vec<_> = after.difference(&before).collect();
    let removed: Vec<_> = before.difference(&after).collect();
    if !added.is_empty() {
        changes.push(format!("{label} now also enables {}", list(added)));
    }
    if !removed.is_empty() {
        changes.push(format!("{label} no longer enables {}", list(removed)));
    }
    if locked(base.1) != locked(head.1) {
        changes.push("Cargo.lock dependencies changed".to_string());
    }
    (!changes.is_empty()).then(|| {
        format!(
            "feature set changed between base and head ({}): {TAIL}",
            changes.join("; ")
        )
    })
}

/// [`shift_note`] for a gate of `common`'s package against `refname`, read
/// from the merge-base's files. `None` when any of them cannot be read.
pub(super) fn shift_note_against(common: &CommonArgs, refname: &str) -> Option<String> {
    let pkg = common.pkg.as_deref()?;
    let base = invoke::git(&["merge-base", "HEAD", refname]).ok()?;
    let top = PathBuf::from(invoke::git(&["rev-parse", "--show-toplevel"]).ok()?);
    let manifest = invoke::pkg_dir(pkg).ok()?.join("Cargo.toml");
    let lock = invoke::workspace_root().ok()?.join("Cargo.lock");
    let at_base = |path: &Path| {
        let rel = path.strip_prefix(&top).ok()?.to_str()?.to_string();
        invoke::git(&["show", &format!("{base}:{rel}")]).ok()
    };
    let head_manifest = std::fs::read_to_string(&manifest).ok()?;
    let head_lock = std::fs::read_to_string(&lock).ok()?;
    shift_note(
        common.features.as_deref(),
        (&at_base(&manifest)?, &at_base(&lock)?),
        (&head_manifest, &head_lock),
    )
}

#[cfg(test)]
mod tests {
    use super::shift_note;

    const BASE: &str = "[features]\nbench-gate = [\"a\"]\na = []\n";
    const HEAD: &str = "[features]\nbench-gate = [\"a\", \"dep:dataframe\"]\na = []\n";
    const LOCK: &str = "[[package]]\nname = \"x\"\nversion = \"1.0.0\"\nsource = \"registry+u\"\n";

    #[test]
    fn a_feature_that_now_enables_more_is_named() {
        let note = shift_note(Some("bench-gate"), (BASE, LOCK), (HEAD, LOCK)).unwrap();
        assert_eq!(
            note,
            "feature set changed between base and head (bench-gate now also enables dataframe): codegen of unchanged code can shift under LTO"
        );
        let back = shift_note(Some("bench-gate"), (HEAD, LOCK), (BASE, LOCK)).unwrap();
        assert!(back.contains("bench-gate no longer enables dataframe"));
    }

    #[test]
    fn changed_locked_dependencies_are_named_and_workspace_members_are_not() {
        let bumped = LOCK.replace("1.0.0", "1.0.1");
        let note = shift_note(None, (BASE, LOCK), (BASE, &bumped)).unwrap();
        assert!(note.contains("Cargo.lock dependencies changed"));
        let member = "[[package]]\nname = \"me\"\nversion = \"0.1.0\"\n";
        let other = member.replace("0.1.0", "0.2.0");
        assert_eq!(shift_note(None, (BASE, member), (BASE, &other)), None);
    }

    #[test]
    fn an_explicit_default_equals_none_and_an_unchanged_graph_is_silent() {
        let table = "[features]\ndefault = [\"a\"]\na = []\n";
        assert_eq!(
            shift_note(Some("default"), (table, LOCK), (table, LOCK)),
            None
        );
        let without = "[features]\na = []\n";
        let with_default = "[features]\ndefault = []\na = []\n";
        assert_eq!(
            shift_note(Some("default"), (without, LOCK), (with_default, LOCK)),
            None
        );
        assert_eq!(shift_note(None, (BASE, LOCK), (BASE, LOCK)), None);
    }
}
