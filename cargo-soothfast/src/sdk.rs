//! `cargo soothfast sdk gen|gate|publish` — generate, gate and publish
//! client SDKs from the same operations the spec emitters render.

use std::path::Path;
use std::process::Command;

use soothfast_sdk::{SdkFileSet, SdkKind, SdkOptions};

use crate::invoke::{self, CommonArgs};
use crate::sdk_config::{self, SdkEntry};
use crate::spec_config::{self, Mode};
use crate::spec_gen;

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("gen") => run_gen(&args[1..]),
        // The SDK surface is derived from the linked generate-mode specs,
        // so gating those specs is gating the SDK.
        Some("gate") => crate::spec_gate::run(&args[1..]),
        Some("publish") => run_publish(&args[1..]),
        _ => {
            eprintln!(
                "soothfast: usage: cargo soothfast sdk gen -p PKG [--check]\n\
                 cargo soothfast sdk gate -p PKG [--base REF]\n\
                 cargo soothfast sdk publish -p PKG [--only LANG] [--dry-run]"
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
            eprintln!("soothfast: unknown sdk gen arg {a:?}");
            return 2;
        }
    }
    let Some(pkg) = common.pkg.clone() else {
        eprintln!("soothfast: sdk gen requires -p PKG");
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

/// One SDK entry's rendered output plus its provenance report.
struct BuiltSdk {
    entry: SdkEntry,
    files: SdkFileSet,
    operations: usize,
    gaps: Vec<String>,
}

/// Emit every configured SDK in memory. Shared by `gen` and `publish`.
fn build_all(
    pkg: &str,
    common: &CommonArgs,
    meta: &invoke::PkgMeta,
) -> Result<Vec<BuiltSdk>, String> {
    let spec_cfg = spec_config::load(&meta.dir)?;
    let sdk_cfg = sdk_config::load(&meta.dir)?;
    if sdk_cfg.entries.is_empty() {
        return Ok(Vec::new());
    }

    for entry in &sdk_cfg.entries {
        let spec_entry = spec_cfg.for_path(&entry.spec);
        if !spec_entry.is_some_and(|e| e.mode == Mode::Generate) {
            return Err(format!(
                "[[sdk]] links spec {:?}, which is not a generate-mode [[spec]] entry — \
                 an SDK generated from a hand-authored spec would desynchronize from it; \
                 set `mode = \"generate\"` on that entry first",
                entry.spec
            ));
        }
    }

    let by_spec = spec_gen::discover_routes(common, None)?;
    let doc = spec_gen::rustdoc_for(pkg, common, None)?;

    let mut built = Vec::new();
    for entry in sdk_cfg.entries {
        let routes = by_spec
            .get(&entry.spec)
            .ok_or_else(|| format!("no #[route] in {pkg} declares spec {:?}", entry.spec))?;
        let spec_entry = spec_cfg.for_path(&entry.spec);
        let (ops, gaps) = spec_gen::operations_for(&doc, spec_entry, routes)?;
        let info = spec_gen::info_for(spec_entry, pkg, meta);
        let opts = sdk_options(&entry, meta);
        let files = entry
            .lang
            .emit(&info, &ops, &opts)
            .map_err(|e| format!("{}: {e}", entry.out))?;
        built.push(BuiltSdk {
            entry,
            files,
            operations: ops.len(),
            gaps,
        });
    }
    Ok(built)
}

fn generate(pkg: &str, common: &CommonArgs, check_only: bool) -> Result<i32, String> {
    let meta = invoke::pkg_meta(pkg).map_err(|e| e.to_string())?;
    let built = build_all(pkg, common, &meta)?;
    if built.is_empty() {
        println!("sdk gen: nothing to generate — no [[sdk]] entry in soothfast.toml");
        return Ok(0);
    }

    let mut stale = 0u32;
    for sdk in &built {
        let out_dir = meta.dir.join(&sdk.entry.out);
        for (rel, content) in &sdk.files.files {
            let target = out_dir.join(rel);
            if check_only {
                let current = std::fs::read_to_string(&target).unwrap_or_default();
                if current != *content {
                    stale += 1;
                    println!(
                        "STALE {}/{rel}: regenerating would change it — run \
                         `cargo soothfast sdk gen -p {pkg}` and commit the result",
                        sdk.entry.out
                    );
                }
            } else {
                spec_gen::write_if_changed(&target, content)?;
            }
        }
        println!(
            "sdk gen: {} [{}] — {} operation(s), {} file(s), {} gap(s), {} note(s)",
            sdk.entry.out,
            sdk.entry.lang.name(),
            sdk.operations,
            sdk.files.files.len(),
            sdk.gaps.len(),
            sdk.files.notes.len(),
        );
        for g in &sdk.gaps {
            println!("  gap: {g}");
        }
        for n in &sdk.files.notes {
            println!("  note: {n}");
        }
    }

    if stale > 0 {
        println!("sdk gen --check: FAILED ({stale} stale file(s))");
        return Ok(1);
    }
    Ok(0)
}

fn sdk_options(entry: &SdkEntry, meta: &invoke::PkgMeta) -> SdkOptions {
    let defaults = SdkOptions::default();
    SdkOptions {
        package: entry.package.clone(),
        module: entry.module(),
        version: entry
            .version
            .clone()
            .unwrap_or_else(|| meta.version.clone()),
        base_url: entry.base_url.clone(),
        description: entry
            .description
            .clone()
            .or_else(|| meta.description.clone()),
        repository: entry.repository.clone(),
        paginated: entry.paginated.clone(),
        cursor_param: entry.cursor_param.clone().unwrap_or(defaults.cursor_param),
        limit_param: entry.limit_param.clone().unwrap_or(defaults.limit_param),
    }
}

fn run_publish(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut only: Option<String> = None;
    let mut dry_run = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--only" => only = it.next().cloned(),
            "--dry-run" => dry_run = true,
            _ if common.try_parse(a, &mut it) => {}
            other => {
                eprintln!("soothfast: unknown sdk publish arg {other:?}");
                return 2;
            }
        }
    }
    let Some(pkg) = common.pkg.clone() else {
        eprintln!("soothfast: sdk publish requires -p PKG");
        return 2;
    };
    match publish(&pkg, &common, only.as_deref(), dry_run) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: {e}");
            1
        }
    }
}

fn publish(
    pkg: &str,
    common: &CommonArgs,
    only: Option<&str>,
    dry_run: bool,
) -> Result<i32, String> {
    let meta = invoke::pkg_meta(pkg).map_err(|e| e.to_string())?;
    let built = build_all(pkg, common, &meta)?;
    let selected: Vec<&BuiltSdk> = built
        .iter()
        .filter(|s| only.is_none_or(|o| s.entry.lang.name() == o))
        .collect();
    if selected.is_empty() {
        println!("sdk publish: nothing to publish");
        return Ok(0);
    }

    for sdk in &selected {
        if let Some(v) = &sdk.entry.version {
            if *v != meta.version {
                return Err(format!(
                    "{}: [[sdk]] version {v} does not match crate version {} — \
                     SDK releases stay in lockstep with the crate",
                    sdk.entry.out, meta.version
                ));
            }
        }
        let out_dir = meta.dir.join(&sdk.entry.out);
        for (rel, content) in &sdk.files.files {
            let on_disk = std::fs::read_to_string(out_dir.join(rel)).unwrap_or_default();
            if on_disk != *content {
                return Err(format!(
                    "{}/{rel} is stale — run `cargo soothfast sdk gen -p {pkg}`, \
                     commit, and publish from that commit",
                    sdk.entry.out
                ));
            }
        }
        match sdk.entry.lang {
            SdkKind::Python => publish_python(&out_dir, dry_run)?,
            SdkKind::TypeScript => {
                return Err("publishing typescript SDKs is not implemented yet".into());
            }
        }
        println!(
            "sdk publish: {} [{}] {}",
            sdk.entry.package,
            sdk.entry.lang.name(),
            if dry_run {
                "built (dry run)"
            } else {
                "published"
            },
        );
    }
    Ok(0)
}

fn publish_python(out_dir: &Path, dry_run: bool) -> Result<(), String> {
    run_tool("uv", &["build"], out_dir)?;
    if !dry_run {
        run_tool("uv", &["publish"], out_dir)?;
    }
    Ok(())
}

fn run_tool(tool: &str, args: &[&str], dir: &Path) -> Result<(), String> {
    let status = Command::new(tool)
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "`{tool}` not found — install it: \
                     curl -LsSf https://astral.sh/uv/install.sh | sh"
                )
            } else {
                format!("cannot run {tool}: {e}")
            }
        })?;
    if !status.success() {
        return Err(format!("`{tool} {}` failed", args.join(" ")));
    }
    Ok(())
}
