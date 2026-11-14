//! Keeping the merge-base worktree on HEAD's locked soothfast versions, so both
//! sides of a comparison embed the same measurement harness.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{git, workspace_root};

/// One `[[package]]` entry from a Cargo.lock.
pub(crate) struct LockPkg {
    pub(crate) name: String,
    pub(crate) version: String,
    /// Registry entries carry a `source` line; path dependencies don't and
    /// cannot be re-pinned.
    pub(crate) registry: bool,
}

pub(crate) fn lock_packages(lock: &str) -> Vec<LockPkg> {
    lock.split("[[package]]")
        .skip(1)
        .filter_map(|block| {
            let field = |key: &str| {
                block
                    .lines()
                    .find_map(|l| l.strip_prefix(key))
                    .map(|v| v.trim().trim_matches('"').to_string())
            };
            Some(LockPkg {
                name: field("name = ")?,
                version: field("version = ")?,
                registry: block.lines().any(|l| l.starts_with("source = ")),
            })
        })
        .collect()
}

/// Soothfast-family registry entries whose worktree version differs from
/// HEAD's: (name, worktree version, HEAD version).
fn harness_mismatches(head_lock: &str, wt_lock: &str) -> Vec<(String, String, String)> {
    let head: BTreeMap<String, LockPkg> = lock_packages(head_lock)
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect();
    lock_packages(wt_lock)
        .into_iter()
        .filter(|p| p.registry && (p.name == "soothfast" || p.name.starts_with("soothfast-")))
        .filter_map(|p| {
            let h = head.get(&p.name).filter(|h| h.registry)?;
            (h.version != p.version).then(|| (p.name, p.version, h.version.clone()))
        })
        .collect()
}

/// HEAD's locked soothfast versions, as `name@version` pairs. The reference
/// side is pinned to these before it is built, so they decide what a
/// reference measurement means.
pub fn harness_versions() -> String {
    let Ok(root) = workspace_root() else {
        return String::new();
    };
    let Ok(lock) = std::fs::read_to_string(root.join("Cargo.lock")) else {
        return String::new();
    };
    let mut out: Vec<String> = lock_packages(&lock)
        .into_iter()
        .filter(|p| p.name == "soothfast" || p.name.starts_with("soothfast-"))
        .map(|p| format!("{}@{}", p.name, p.version))
        .collect();
    out.sort();
    out.join(",")
}

/// Whether the working tree has no uncommitted changes, so HEAD's SHA names
/// what was actually measured. The store and the directories the CI action
/// checks out into the workspace do not count: no build reads them.
pub fn tree_is_clean() -> bool {
    git(&["status", "--porcelain"]).is_ok_and(|s| only_scratch(&s))
}

/// Whether every path `git status --porcelain` lists is scratch.
fn only_scratch(porcelain: &str) -> bool {
    porcelain
        .lines()
        .filter_map(|line| line.get(3..))
        .flat_map(|paths| paths.split(" -> "))
        .all(|path| crate::store::is_scratch(std::path::Path::new(path.trim_matches('"'))))
}

/// Whether the reference side ended up embedding HEAD's measurement harness.
/// A reference built against another harness measures a different protocol,
/// so the outcome travels with the run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarnessSync {
    /// Crates cargo would not move, each spelled `name 0.2.0 -> 0.3.1`.
    unpinned: Vec<String>,
    /// The callgrind methods of the reference and of HEAD, when they differ
    /// or the reference's is unknown.
    method: Option<(String, String)>,
}

impl HarnessSync {
    /// Rebuild an outcome from the crate labels a stored run kept.
    pub fn from_unpinned(unpinned: Vec<String>) -> Self {
        HarnessSync {
            unpinned,
            method: None,
        }
    }

    /// The crates left at a version of their own, empty when the pin held.
    pub fn unpinned(&self) -> &[String] {
        &self.unpinned
    }

    /// This outcome with the callgrind methods of the two sides.
    pub fn with_method(self, base: &str, head: &str) -> Self {
        HarnessSync {
            method: Some((base.into(), head.into())),
            ..self
        }
    }

    /// The stored form of [`HarnessSync::with_method`]: `[base, head]`.
    pub fn method(&self) -> Option<[&str; 2]> {
        self.method.as_ref().map(|(b, h)| [b.as_str(), h.as_str()])
    }

    /// The method difference, spelled for a reader, when there is one.
    pub fn method_note(&self) -> Option<String> {
        self.method().map(|[base, head]| {
            format!("base measured with callgrind method {base}, head with {head}")
        })
    }

    /// Whether the two sides embed different measurement harnesses.
    pub fn is_mismatched(&self) -> bool {
        !self.unpinned.is_empty() || self.method.is_some()
    }
}

fn sync_outcome(mismatches: &[(String, String, String)]) -> HarnessSync {
    HarnessSync::from_unpinned(
        mismatches
            .iter()
            .map(|(name, from, to)| format!("{name} {from} -> {to}"))
            .collect(),
    )
}

/// Pin the merge-base worktree's soothfast crates to HEAD's locked versions
/// so the reference bench binary embeds the same measurement harness. A pin
/// the worktree's own requirement excludes is retried after rewriting that
/// requirement; anything still unpinned comes back as `Mismatched` for the
/// caller to put in front of the verdict.
pub fn sync_harness_versions(wt: &Path) -> Result<HarnessSync, String> {
    let head_path = workspace_root()
        .map_err(|e| e.to_string())?
        .join("Cargo.lock");
    let Ok(head) = std::fs::read_to_string(&head_path) else {
        return Ok(HarnessSync::default());
    };
    // One crate per pass, re-reading the lock: pinning the facade drags its
    // family along, and already-moved entries must not be warned about.
    for _ in 0..16 {
        let Ok(base) = std::fs::read_to_string(wt.join("Cargo.lock")) else {
            return Ok(HarnessSync::default());
        };
        let mismatches = harness_mismatches(&head, &base);
        let Some((name, from, to)) = mismatches.first().cloned() else {
            return Ok(HarnessSync::default());
        };
        println!(
            "gate: pinning {name} {from} -> {to} in the merge-base worktree (harness must match HEAD)"
        );
        let mut refused = pin_precise(wt, &name, &from, &to)?;
        // A caret-incompatible bump is refused by the merge-base's own
        // requirement. The worktree is disposable, so widen it and retry.
        if refused.as_deref().is_some_and(is_requirement_conflict)
            && relax_harness_requirements(wt, &mismatches)?
        {
            println!("gate: widened the merge-base's soothfast requirements to retry the pin");
            refused = pin_precise(wt, &name, &from, &to)?;
        }
        if let Some(stderr) = refused {
            eprintln!(
                "WARN: could not pin {name} ({}); the reference keeps its own harness — deltas may reflect the harness change itself",
                stderr.trim().lines().last().unwrap_or("no error output")
            );
            return Ok(sync_outcome(&mismatches));
        }
    }
    let remaining = std::fs::read_to_string(wt.join("Cargo.lock"))
        .map(|base| harness_mismatches(&head, &base))
        .unwrap_or_default();
    Ok(sync_outcome(&remaining))
}

/// Cargo's precise pin, run in the worktree. `Ok(None)` when it landed,
/// `Ok(Some(stderr))` when cargo refused it.
fn pin_precise(wt: &Path, name: &str, from: &str, to: &str) -> Result<Option<String>, String> {
    let out = Command::new("cargo")
        .args(["update", "-p", &format!("{name}@{from}"), "--precise", to])
        .current_dir(wt)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&out.stderr).into_owned()))
}

/// Whether cargo refused the pin because a requirement excludes the wanted
/// version, rather than because the index or network was unreachable.
fn is_requirement_conflict(stderr: &str) -> bool {
    stderr.contains("didn't match")
        || stderr.contains("does not match")
        || stderr.contains("failed to select a version")
}

/// Point the worktree's soothfast-family requirements at the versions HEAD
/// locks, so the pin above is no longer refused. Returns whether any
/// manifest changed.
fn relax_harness_requirements(
    wt: &Path,
    mismatches: &[(String, String, String)],
) -> Result<bool, String> {
    let wanted: BTreeMap<&str, &str> = mismatches
        .iter()
        .map(|(name, _, to)| (name.as_str(), to.as_str()))
        .collect();
    let mut changed = false;
    for path in manifests(wt) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(next) = retarget_requirements(&text, &wanted) else {
            continue;
        };
        std::fs::write(&path, next).map_err(|e| format!("{}: {e}", path.display()))?;
        changed = true;
    }
    Ok(changed)
}

/// Every `Cargo.toml` under `dir`. `target`/`.git` hold nothing the
/// resolver reads.
fn manifests(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            out.extend(manifests(&path));
        } else if name == "Cargo.toml" {
            out.push(path);
        }
    }
    out
}

/// Rewrite the `version` requirement of each dependency `wanted` names, in
/// one manifest's text. `None` when nothing matched. Only the requirement
/// string moves: re-specifying the dependency would drop `features`,
/// `optional` or `default-features` beside it.
fn retarget_requirements(text: &str, wanted: &BTreeMap<&str, &str>) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut table = String::new();
    let mut changed = false;
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches('\n').trim_end_matches('\r');
        let ending = &raw[line.len()..];
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            table = trimmed
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .to_string();
            out.push_str(raw);
            continue;
        }
        let rewritten = match dep_table_name(&table) {
            Some(name) => version_key(line)
                .zip(wanted.get(name))
                .and_then(|(at, to)| retarget_line(line, at, to)),
            None => dep_line(line, &table, wanted).and_then(|(at, to)| retarget_line(line, at, to)),
        };
        match rewritten {
            Some(next) => {
                out.push_str(&next);
                out.push_str(ending);
                changed = true;
            }
            None => out.push_str(raw),
        }
    }
    changed.then_some(out)
}

/// Where a `version = "..."` line's value starts.
fn version_key(line: &str) -> Option<usize> {
    let (key, rest) = line.split_once('=')?;
    (key.trim().trim_matches('"') == "version").then(|| line.len() - rest.len())
}

/// The dependency a `[…dependencies.NAME]` header names.
fn dep_table_name(table: &str) -> Option<&str> {
    let (parent, name) = table.rsplit_once('.')?;
    is_dep_table(parent).then_some(name)
}

fn is_dep_table(table: &str) -> bool {
    matches!(
        table.rsplit('.').next(),
        Some("dependencies" | "dev-dependencies" | "build-dependencies")
    )
}

/// Where a dependency line's requirement starts, and what it should become.
/// A path or git dependency is left alone: its version is not what the
/// resolver picks.
fn dep_line<'a>(
    line: &str,
    table: &str,
    wanted: &BTreeMap<&str, &'a str>,
) -> Option<(usize, &'a str)> {
    if !is_dep_table(table) {
        return None;
    }
    let (key, rest) = line.split_once('=')?;
    let to = wanted.get(key.trim().trim_matches('"'))?;
    let rest_at = line.len() - rest.len();
    let value = rest.trim_start();
    if value.starts_with('"') {
        return Some((rest_at, to));
    }
    if !value.starts_with('{') || value.contains("path =") || value.contains("git =") {
        return None;
    }
    let version_at = rest_at + rest.find("version")?;
    Some((version_at, to))
}

/// Replace the first quoted string at or after `from` with `to`, leaving
/// the rest of the line (other keys, a trailing comment) untouched.
fn retarget_line(line: &str, from: usize, to: &str) -> Option<String> {
    let open = from + line.get(from..)?.find('"')?;
    let close = open + 1 + line.get(open + 1..)?.find('"')?;
    (&line[open + 1..close] != to)
        .then(|| format!("{}\"{to}\"{}", &line[..open], &line[close + 1..]))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{harness_mismatches, only_scratch};

    #[test]
    fn the_store_and_the_action_checkout_leave_the_tree_clean() {
        assert!(only_scratch(""));
        assert!(only_scratch(
            "?? .soothfast-action/\n M .soothfast/trend.jsonl\n"
        ));
        assert!(!only_scratch("?? .soothfast-action/\n M src/lib.rs\n"));
        assert!(!only_scratch("R  .soothfast/a.rs -> src/a.rs\n"));
        assert!(!only_scratch("?? \"docs/a b.md\"\n"));
    }

    #[test]
    fn a_method_difference_mismatches_the_harness_without_a_pin() {
        let sync = super::HarnessSync::default().with_method("two-process", "one-process");
        assert!(sync.is_mismatched());
        assert!(sync.unpinned().is_empty());
        assert_eq!(
            sync.method_note().as_deref(),
            Some("base measured with callgrind method two-process, head with one-process")
        );
    }

    const HEAD: &str = r#"
version = 4

[[package]]
name = "serde"
version = "1.0.200"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "soothfast"
version = "0.1.7"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "soothfast-measure",
]

[[package]]
name = "soothfast-measure"
version = "0.1.7"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

    #[test]
    fn pins_family_crates_to_heads_versions() {
        let base = HEAD.replace("0.1.7", "0.1.5");
        assert_eq!(
            harness_mismatches(HEAD, &base),
            vec![
                ("soothfast".into(), "0.1.5".into(), "0.1.7".into()),
                ("soothfast-measure".into(), "0.1.5".into(), "0.1.7".into()),
            ]
        );
    }

    #[test]
    fn matching_locks_and_foreign_crates_need_nothing() {
        assert!(harness_mismatches(HEAD, HEAD).is_empty());
        // serde is measured code, not the harness — its bumps must stay gated.
        let base = HEAD.replace("1.0.200", "1.0.100");
        assert!(harness_mismatches(HEAD, &base).is_empty());
    }

    #[test]
    fn path_dependencies_are_skipped() {
        // Gating soothfast itself: family crates are path deps, no source line.
        let no_source = HEAD.replace(
            "source = \"registry+https://github.com/rust-lang/crates.io-index\"",
            "",
        );
        let base = no_source.replace("0.1.7", "0.1.5");
        assert!(harness_mismatches(&no_source, &base).is_empty());
    }

    #[test]
    fn a_caret_incompatible_bump_is_still_a_mismatch() {
        let base = HEAD.replace("0.1.7", "0.2.0");
        assert_eq!(
            harness_mismatches(HEAD, &base),
            vec![
                ("soothfast".into(), "0.2.0".into(), "0.1.7".into()),
                ("soothfast-measure".into(), "0.2.0".into(), "0.1.7".into()),
            ]
        );
    }

    fn wanted() -> BTreeMap<&'static str, &'static str> {
        BTreeMap::from([("soothfast", "0.3.1")])
    }

    #[test]
    fn a_requirement_conflict_is_told_apart_from_an_unreachable_index() {
        assert!(super::is_requirement_conflict(
            "error: failed to select a version for the requirement `soothfast = \"^0.2.0\"`\n\
             candidate versions found which didn't match: 0.3.1\n"
        ));
        assert!(!super::is_requirement_conflict(
            "error: failed to get `soothfast` as a dependency\n\
             Caused by: network failure seems to have happened\n"
        ));
    }

    #[test]
    fn every_requirement_shape_moves_to_heads_version() {
        let manifest = r#"[package]
name = "demo"

[dependencies]
soothfast = "0.2.0"
serde = "1.0.200"

[dev-dependencies]
soothfast = { version = "0.2.0", features = ["runner"], default-features = false }

[target.'cfg(unix)'.dependencies.soothfast]
version = "0.2.0"
optional = true
"#;
        let got = super::retarget_requirements(manifest, &wanted()).expect("rewritten");
        assert_eq!(got.matches("\"0.3.1\"").count(), 3);
        assert!(!got.contains("0.2.0"));
        assert!(got.contains("features = [\"runner\"], default-features = false"));
        assert!(got.contains("serde = \"1.0.200\""));
        assert!(got.contains("optional = true"));
    }

    #[test]
    fn a_manifest_with_nothing_to_move_is_left_alone() {
        let manifest = "[dependencies]\nsoothfast = \"0.3.1\"\nserde = \"1\"\n";
        assert!(super::retarget_requirements(manifest, &wanted()).is_none());
        // A path dep resolves from the tree, not from a requirement.
        let path_dep =
            "[dependencies]\nsoothfast = { path = \"../soothfast\", version = \"0.2.0\" }\n";
        assert!(super::retarget_requirements(path_dep, &wanted()).is_none());
        // `soothfast` outside a dependency table names something else.
        let other = "[features]\nsoothfast = \"0.2.0\"\n";
        assert!(super::retarget_requirements(other, &wanted()).is_none());
    }

    #[test]
    fn rewriting_keeps_the_rest_of_the_line() {
        let manifest = "[dependencies]\nsoothfast = \"0.2.0\" # the harness\n";
        let got = super::retarget_requirements(manifest, &wanted()).expect("rewritten");
        assert_eq!(got, "[dependencies]\nsoothfast = \"0.3.1\" # the harness\n");
    }

    #[test]
    fn an_unpinned_crate_survives_as_the_runs_verdict_context() {
        let matched = super::sync_outcome(&[]);
        assert!(!matched.is_mismatched());
        assert!(matched.unpinned().is_empty());
        let missed = super::sync_outcome(&[("soothfast".into(), "0.2.0".into(), "0.3.1".into())]);
        assert!(missed.is_mismatched());
        assert_eq!(missed.unpinned(), ["soothfast 0.2.0 -> 0.3.1"]);
        assert_eq!(
            super::HarnessSync::from_unpinned(missed.unpinned().to_vec()),
            missed
        );
    }
}
