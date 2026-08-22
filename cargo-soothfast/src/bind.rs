//! `cargo soothfast bind` — native language bindings from the code.
//!
//! The SDK family's sibling for surfaces with no wire boundary. The exported
//! surface is walked once and lowered per language, so a class defined once
//! in Rust reaches every configured language as the same class.

use std::path::Path;

use soothfast_bind::foreign::TypeTable;
use soothfast_bind::model::Surface;
use soothfast_bind::{BindFileSet, BindOptions, compat};

use crate::bind_config::{self, BindEntry};
use crate::invoke::{self, CommonArgs};
use crate::spec_gen;

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("gen") => run_gen(&args[1..]),
        Some("gate") => run_gate(&args[1..]),
        Some("build") => run_build(&args[1..]),
        _ => {
            eprintln!(
                "soothfast: usage: cargo soothfast bind gen -p PKG [--check]\n\
                 cargo soothfast bind gate -p PKG [--base REF] [--allow-breaking]\n\
                 cargo soothfast bind build -p PKG [--target TRIPLE].. [--debug]"
            );
            2
        }
    }
}

fn run_gen(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut check_only = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--check" {
            check_only = true;
        } else if !common.try_parse(a, &mut it) {
            eprintln!("soothfast: unknown bind gen arg {a:?}");
            return 2;
        }
    }
    let Some(pkg) = common.pkg.clone() else {
        eprintln!("soothfast: bind gen requires -p PKG");
        return 2;
    };
    match generate(&pkg, &common, check_only) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: {e}");
            1
        }
    }
}

fn run_gate(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut base = "origin/master".to_string();
    let mut allow_breaking = false;
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
            "--allow-breaking" => allow_breaking = true,
            _ if common.try_parse(a, &mut it) => {}
            _ => {
                eprintln!("soothfast: unknown bind gate arg {a:?}");
                return 2;
            }
        }
    }
    let Some(pkg) = common.pkg.clone() else {
        eprintln!("soothfast: bind gate requires -p PKG");
        return 2;
    };
    match gate(&pkg, &common, &base, allow_breaking) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: {e}");
            1
        }
    }
}

fn gate(pkg: &str, common: &CommonArgs, base: &str, allow_breaking: bool) -> Result<i32, String> {
    let meta = invoke::pkg_meta(pkg).map_err(|e| e.to_string())?;
    if bind_config::load(&meta.dir)?.entries.is_empty() {
        println!("bind gate: no [[bind]] entry — nothing to gate");
        return Ok(0);
    }
    let (head, _) = exported_surface(pkg, common)?;
    let (base_surface, _) =
        invoke::with_merge_base_worktree(base, |wt| base_surface_in(pkg, common, wt))?;

    let changes = compat::diff(&base_surface, &head);
    if changes.is_empty() {
        println!("bind gate: no binding surface changes vs {base}");
        return Ok(0);
    }
    let breaking = changes.iter().filter(|c| c.breaking()).count();
    for change in &changes {
        let label = if change.breaking() { "BREAK" } else { "add  " };
        println!("{label} {}", change.explain());
    }
    if breaking == 0 {
        println!("bind gate: {} additive change(s) vs {base}", changes.len());
        return Ok(0);
    }
    if allow_breaking {
        println!("bind gate: {breaking} breaking change(s), allowed by --allow-breaking");
        return Ok(0);
    }
    println!(
        "bind gate: FAILED ({breaking} breaking change(s) vs {base}) — \
         release it deliberately with --allow-breaking"
    );
    Ok(1)
}

/// The surface as of the merge base, walked inside its own worktree.
fn base_surface_in(
    pkg: &str,
    common: &CommonArgs,
    wt: &Path,
) -> Result<(Surface, Vec<soothfast_bind::gap::Gap>), String> {
    let records = spec_gen::discover_exports(common, Some(wt))?;
    if records.is_empty() {
        return Ok((Surface::default(), Vec::new()));
    }
    let doc = spec_gen::rustdoc_for(pkg, common, Some(wt))?;
    soothfast_bind::walk::surface(&doc, &TypeTable::with_defaults(), &records)
}

fn run_build(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut targets: Vec<String> = Vec::new();
    let mut release = true;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => match it.next() {
                Some(t) => targets.push(t.clone()),
                None => {
                    eprintln!("soothfast: --target needs a triple");
                    return 2;
                }
            },
            "--debug" => release = false,
            _ if common.try_parse(a, &mut it) => {}
            _ => {
                eprintln!("soothfast: unknown bind build arg {a:?}");
                return 2;
            }
        }
    }
    let Some(pkg) = common.pkg.clone() else {
        eprintln!("soothfast: bind build requires -p PKG");
        return 2;
    };
    match build(&pkg, &targets, release) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: {e}");
            1
        }
    }
}

/// One entry's rendered output.
struct Built {
    entry: BindEntry,
    files: BindFileSet,
}

/// Render every configured entry from one exported surface.
///
/// The surface is walked once and lowered per language, which is what makes
/// a class defined once in Rust reach every configured language as the same
/// class.
fn build_all(
    pkg: &str,
    common: &CommonArgs,
    dir: &Path,
    version: &str,
) -> Result<Vec<Built>, String> {
    let cfg = bind_config::load(dir)?;
    if cfg.entries.is_empty() {
        return Ok(Vec::new());
    }

    let (surface, gaps) = exported_surface(pkg, common)?;
    let mut out = Vec::new();
    for entry in cfg.entries {
        let opts = bind_options(&entry, pkg, version);
        let files = entry.lang.emit(&surface, gaps.clone(), &opts)?;
        out.push(Built { entry, files });
    }
    Ok(out)
}

/// Discover the exported items, then read their shapes out of rustdoc.
pub(crate) fn exported_surface(
    pkg: &str,
    common: &CommonArgs,
) -> Result<(Surface, Vec<soothfast_bind::gap::Gap>), String> {
    let records = spec_gen::discover_exports(common, None)?;
    if records.is_empty() {
        // linkme registrations reach the bench binary only if the linker
        // keeps the library, which it does only when something names it.
        return Err(format!(
            "no `#[soothfast::export]` items registered — if the annotations are \
             in the library, add `use {} as _;` to its bench target so the \
             linker keeps their registrations",
            pkg.replace('-', "_")
        ));
    }
    let doc = spec_gen::rustdoc_for(pkg, common, None)?;
    soothfast_bind::walk::surface(&doc, &TypeTable::with_defaults(), &records)
}

fn bind_options(entry: &BindEntry, pkg: &str, version: &str) -> BindOptions {
    BindOptions {
        package: entry.package.clone(),
        module: entry.module(),
        version: entry.version.clone().unwrap_or_else(|| version.to_string()),
        crate_name: pkg.replace('-', "_"),
        crate_package: pkg.to_string(),
        crate_path: crate_path(&entry.out),
        description: entry.description.clone(),
        repository: entry.repository.clone(),
        targets: entry.targets.clone(),
        backend_version: entry.backend_version.clone(),
    }
}

/// The path from the glue crate back to the package it binds, one `..` per
/// segment of the output directory.
fn crate_path(out: &str) -> String {
    let depth = out.split('/').filter(|s| !s.is_empty()).count().max(1);
    vec![".."; depth].join("/")
}

fn generate(pkg: &str, common: &CommonArgs, check_only: bool) -> Result<i32, String> {
    let meta = invoke::pkg_meta(pkg).map_err(|e| e.to_string())?;
    let built = build_all(pkg, common, &meta.dir, &meta.version)?;
    if built.is_empty() {
        println!("bind gen: nothing to generate — no [[bind]] entry in soothfast.toml");
        return Ok(0);
    }

    let mut stale = 0u32;
    for bound in &built {
        let out_dir = meta.dir.join(&bound.entry.out);
        for (rel, content) in &bound.files.files {
            let target = out_dir.join(rel);
            if check_only {
                let current = std::fs::read_to_string(&target).unwrap_or_default();
                if current != *content {
                    stale += 1;
                    println!(
                        "STALE {}/{rel}: regenerating would change it — run \
                         `cargo soothfast bind gen -p {pkg}` and commit the result",
                        bound.entry.out
                    );
                }
            } else {
                spec_gen::write_if_changed(&target, content)?;
            }
        }
        println!(
            "bind gen: {} [{}] — {} file(s), {} gap(s), {} note(s)",
            bound.entry.out,
            bound.entry.lang.name(),
            bound.files.files.len(),
            bound.files.gaps.len(),
            bound.files.notes.len(),
        );
        for g in &bound.files.gaps {
            println!("  gap: {g}");
        }
        for n in &bound.files.notes {
            println!("  note: {n}");
        }
    }

    if stale > 0 {
        println!("bind gen --check: FAILED ({stale} stale file(s))");
        return Ok(1);
    }
    Ok(0)
}

fn build(pkg: &str, targets: &[String], release: bool) -> Result<i32, String> {
    let meta = invoke::pkg_meta(pkg).map_err(|e| e.to_string())?;
    let cfg = bind_config::load(&meta.dir)?;
    if cfg.entries.is_empty() {
        println!("bind build: nothing to build — no [[bind]] entry in soothfast.toml");
        return Ok(0);
    }
    let mut failed = 0u32;
    for entry in &cfg.entries {
        let glue = meta.dir.join(&entry.out);
        let wanted = match targets.is_empty() {
            true => entry.targets.clone(),
            false => targets.to_vec(),
        };
        match crate::bind_build::run(entry.lang, &glue, &wanted, release) {
            Ok(artifacts) => {
                println!(
                    "bind build: {} [{}] — {} artifact(s)",
                    entry.out,
                    entry.lang.name(),
                    artifacts.len()
                );
                for a in &artifacts {
                    println!("  {a}");
                }
            }
            Err(e) => {
                failed += 1;
                println!(
                    "bind build: {} [{}] FAILED: {e}",
                    entry.out,
                    entry.lang.name()
                );
            }
        }
    }
    Ok(if failed > 0 { 1 } else { 0 })
}
