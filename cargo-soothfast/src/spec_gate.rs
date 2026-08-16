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

use soothfast_spec::compat::Severity;

use crate::invoke::{self, CommonArgs};
use crate::spec_config;
use crate::spec_gen;

pub fn run(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut base = "origin/master".to_string();
    let mut allow_breaking = false;
    let mut from_committed = false;
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
            "--from-committed" => from_committed = true,
            _ => {
                if !common.try_parse(a, &mut it) {
                    eprintln!("soothfast: unknown spec gate arg {a:?}");
                    return 2;
                }
            }
        }
    }
    let Some(pkg) = common.pkg.clone() else {
        eprintln!("soothfast: spec gate requires -p PKG");
        return 2;
    };
    match gate(&pkg, &common, &base, allow_breaking, from_committed) {
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
    from_committed: bool,
) -> Result<i32, String> {
    let meta = invoke::pkg_meta(pkg).map_err(|e| e.to_string())?;
    let cfg = spec_config::load(&meta.dir)?;

    let head = spec_gen::build(pkg, common, None, &cfg, &meta)?;
    if head.docs.is_empty() {
        println!("spec gate: no generated spec files — nothing to gate");
        return Ok(0);
    }

    let base_docs = if from_committed {
        committed_base_docs(&head, &cfg, &meta, base)?
    } else {
        invoke::with_merge_base_worktree(base, |wt| {
            spec_gen::build(pkg, common, Some(wt), &cfg, &meta)
        })?
        .docs
    };

    let mut breaking = 0u32;
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
        for c in &changes {
            let tag = match c.severity {
                Severity::Breaking => {
                    breaking += 1;
                    "BREAKING"
                }
                Severity::Additive => {
                    additive += 1;
                    "additive"
                }
            };
            println!("  {tag}  {}: {}", c.at, c.detail);
        }
    }

    println!("spec gate: {breaking} breaking, {additive} additive");
    if breaking > 0 && !allow_breaking {
        println!(
            "spec gate: FAILED — pass --allow-breaking to release these \
             deliberately, after coordinating with consumers"
        );
        return Ok(1);
    }
    if breaking > 0 {
        println!("spec gate: breaking changes allowed by --allow-breaking");
    }
    Ok(0)
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
