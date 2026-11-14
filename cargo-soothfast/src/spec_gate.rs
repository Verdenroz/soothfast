//! `cargo soothfast spec gate` — fail on API changes that break consumers.
//!
//! A generated spec can never disagree with the code, so the drift check that
//! `spec check` performs stops being meaningful for files we generate. What
//! remains worth gating is compatibility: the spec is rebuilt from the
//! merge-base in a temporary worktree and compared against this branch's, so
//! there is no committed baseline to go stale.
//!
//! `--from-committed` skips the worktree rebuild and reads the committed
//! spec files at the merge-base instead. Every parsed file must re-render
//! byte-identically, so the mode is self-checking; it is the right default
//! for CI that already requires the freshness check on every merge.
//! `--prefer-committed` reads them the same way and rebuilds the merge-base
//! when one does not re-render, as a file written by an older release may not.
//!
//! A deliberate break is acknowledged the way consumers experience it: by
//! the spec's own version. Breaking findings pass when the head spec's
//! `info.version` is a semver-major bump over the base's (minor for 0.x),
//! and fail otherwise. The bump lever is `version` on the `[[spec]]`
//! entry, or the crate version it defaults to. Dialects without a version
//! field (GraphQL, MCP tools) keep failing unless `--allow-breaking`
//! says the break is coordinated.

use soothfast_spec::compat::Severity;

use crate::config::{self, Group};
use crate::invoke::{self, CommonArgs};
use crate::spec_config;
use crate::spec_gen;

/// Where the base documents come from.
#[derive(Clone, Copy, Debug, PartialEq)]
enum BaseSource {
    /// Generated from the merge-base in a worktree.
    Rebuild,
    /// The committed spec files at the merge-base, or an error.
    Committed,
    /// The committed files when they re-render, a rebuild otherwise.
    PreferCommitted,
}

impl BaseSource {
    fn from_flag(flag: &str) -> Option<Self> {
        match flag {
            "--from-committed" => Some(Self::Committed),
            "--prefer-committed" => Some(Self::PreferCommitted),
            _ => None,
        }
    }
}

pub fn run(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut base = "origin/master".to_string();
    let mut allow_breaking = false;
    let mut source = BaseSource::Rebuild;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--base" => match it.next() {
                Some(b) => base = b.clone(),
                None => {
                    eprintln!("soothfast: --base needs a git ref");
                    return 2;
                }
            },
            // Escape hatch for a deliberate, coordinated break.
            "--allow-breaking" => allow_breaking = true,
            _ => {
                if let Some(named) = BaseSource::from_flag(a) {
                    source = named;
                } else if !common.try_parse(a, &mut it) {
                    eprintln!("soothfast: unknown spec gate arg {a:?}");
                    return 2;
                }
            }
        }
    }
    if let Err(e) = config::apply(&mut common, Group::Spec) {
        eprintln!("soothfast: {e}");
        return 2;
    }
    let Some(pkg) = common.pkg.clone() else {
        eprintln!("soothfast: spec gate requires -p PKG");
        return 2;
    };
    match gate(&pkg, &common, &base, allow_breaking, source) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: {e}");
            1
        }
    }
}

fn gate(
    pkg: &str,
    common: &CommonArgs,
    base: &str,
    allow_breaking: bool,
    source: BaseSource,
) -> Result<i32, String> {
    let meta = invoke::pkg_meta(pkg).map_err(|e| e.to_string())?;
    let cfg = spec_config::load(&meta.dir)?;

    let head = spec_gen::build(pkg, common, None, &cfg, &meta)?;
    if head.docs.is_empty() {
        println!("spec gate: no generated spec files — nothing to gate");
        return Ok(0);
    }

    let rebuilt = || {
        invoke::with_merge_base_worktree(base, |wt| {
            spec_gen::build(pkg, common, Some(wt), &cfg, &meta)
        })
        .map(|built| built.docs)
    };
    let base_docs = match source {
        BaseSource::Rebuild => rebuilt()?,
        BaseSource::Committed => committed_base_docs(&head, &cfg, &meta, base)?,
        BaseSource::PreferCommitted => match committed_base_docs(&head, &cfg, &meta, base) {
            Ok(docs) => docs,
            Err(e) => {
                println!("spec gate: {e}");
                println!("spec gate: rebuilding the merge-base instead");
                rebuilt()?
            }
        },
    };

    let mut breaking = 0u32;
    let mut acknowledged = 0u32;
    let mut additive = 0u32;

    for (spec_file, new_doc) in &head.docs {
        let kind = cfg.dialect_of(spec_file);
        // A spec file absent from the base is entirely new, so every
        // operation in it is additive by construction.
        let empty = kind.empty();
        let old_doc = base_docs.get(spec_file).unwrap_or(&empty);
        let changes = kind.diff(old_doc, new_doc);

        if changes.is_empty() {
            println!(
                "spec gate: {spec_file} [{}] — no API changes vs {base}",
                kind.name()
            );
            continue;
        }
        println!("spec gate: {spec_file} [{}] — vs {base}", kind.name());
        let mut file_breaking = 0u32;
        for c in &changes {
            let tag = match c.severity {
                Severity::Breaking => {
                    file_breaking += 1;
                    "BREAKING"
                }
                Severity::Additive => {
                    additive += 1;
                    "additive"
                }
            };
            println!("  {tag}  {}: {}", c.at, c.detail);
        }
        if file_breaking > 0 {
            match (doc_version(old_doc), doc_version(new_doc)) {
                (Some(base_v), Some(head_v)) if bump_acknowledges(base_v, head_v) => {
                    acknowledged += file_breaking;
                    println!("  version bump {base_v} → {head_v} acknowledges the break");
                }
                (Some(base_v), Some(head_v)) => {
                    breaking += file_breaking;
                    println!(
                        "  spec version {head_v} does not acknowledge the break (base {base_v})"
                    );
                }
                _ => breaking += file_breaking,
            }
        }
    }

    println!("spec gate: {breaking} breaking, {acknowledged} acknowledged, {additive} additive");
    if breaking > 0 && !allow_breaking {
        println!(
            "spec gate: FAILED — a deliberate break needs a semver-major bump of \
             the spec version (`version` on the [[spec]] entry, or the crate \
             version it defaults to), or --allow-breaking"
        );
        return Ok(1);
    }
    if breaking > 0 {
        println!("spec gate: breaking changes allowed by --allow-breaking");
    }
    Ok(0)
}

/// The spec document's own version: `info.version` where the dialect has
/// one (OpenAPI, AsyncAPI).
fn doc_version(doc: &serde_json::Value) -> Option<&str> {
    doc.get("info")?.get("version")?.as_str()
}

/// Whether `head` is a semver bump that licenses breaking changes over
/// `base`: a major bump, or a minor bump while the major is 0 (Cargo's
/// 0.x convention). Pre-release and build suffixes are ignored.
fn bump_acknowledges(base: &str, head: &str) -> bool {
    let (Some(base), Some(head)) = (core_triple(base), core_triple(head)) else {
        return false;
    };
    if base.0 == 0 && head.0 == 0 {
        return head.1 > base.1;
    }
    head.0 > base.0
}

fn core_triple(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(str::parse);
    Some((
        parts.next()?.ok()?,
        parts.next().unwrap_or(Ok(0)).ok()?,
        parts.next().unwrap_or(Ok(0)).ok()?,
    ))
}

/// The base documents, read from the committed spec files at the merge-base
/// instead of rebuilding it. Sound whenever merges require the freshness
/// check, which guarantees a committed generated spec matches its code.
/// Files absent at the base diff as new, the same as the worktree path.
fn committed_base_docs(
    head: &spec_gen::Built,
    cfg: &spec_config::SpecConfig,
    meta: &invoke::PkgMeta,
    base: &str,
) -> Result<std::collections::BTreeMap<String, serde_json::Value>, String> {
    let sha = invoke::git(&["merge-base", "HEAD", base]).map_err(|e| e.to_string())?;
    println!(
        "spec gate: base from committed specs at {}",
        &sha[..sha.len().min(12)]
    );
    let mut docs = std::collections::BTreeMap::new();
    for spec_file in head.docs.keys() {
        let rev = format!("{sha}:./{spec_file}");
        let Ok(text) = invoke::git_in(&meta.dir, &["show", &rev]) else {
            continue;
        };
        let value = cfg.dialect_of(spec_file).parse(spec_file, &text)?;
        docs.insert(spec_file.clone(), value);
    }
    Ok(docs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_flag_names_its_base_source() {
        assert_eq!(
            BaseSource::from_flag("--from-committed"),
            Some(BaseSource::Committed)
        );
        assert_eq!(
            BaseSource::from_flag("--prefer-committed"),
            Some(BaseSource::PreferCommitted)
        );
        assert_eq!(BaseSource::from_flag("--base"), None);
    }

    #[test]
    fn a_major_bump_licenses_a_break_and_lesser_bumps_do_not() {
        assert!(bump_acknowledges("2.8.0", "3.0.0"));
        assert!(bump_acknowledges("2.8.0", "4.1.2"));
        assert!(!bump_acknowledges("2.8.0", "2.9.0"));
        assert!(!bump_acknowledges("2.8.0", "2.8.1"));
        assert!(!bump_acknowledges("2.8.0", "2.8.0"));
        assert!(!bump_acknowledges("3.0.0", "2.0.0"));
    }

    #[test]
    fn zero_major_uses_the_minor_as_the_breaking_axis() {
        assert!(bump_acknowledges("0.1.12", "0.2.0"));
        assert!(!bump_acknowledges("0.1.12", "0.1.13"));
        assert!(bump_acknowledges("0.1.12", "1.0.0"));
    }

    #[test]
    fn suffixes_are_ignored_and_garbage_never_acknowledges() {
        assert!(bump_acknowledges("2.8.0", "3.0.0-rc.1"));
        assert!(!bump_acknowledges("two", "3.0.0"));
        assert!(!bump_acknowledges("2.8.0", "next"));
    }

    #[test]
    fn doc_version_reads_info_version_only() {
        let doc = serde_json::json!({"info": {"version": "2.8.0"}});
        assert_eq!(doc_version(&doc), Some("2.8.0"));
        assert_eq!(doc_version(&serde_json::json!({"version": "1"})), None);
    }
}
