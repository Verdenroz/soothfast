//! `cargo soothfast sdk gen|gate|publish` — generate, gate and publish
//! client SDKs from the same operations the spec emitters render.

use std::path::Path;
use std::process::Command;

use soothfast_sdk::target::Target;
use soothfast_sdk::{SdkFileSet, SdkKind, SdkOptions};
use soothfast_spec::schema::Docs;

use crate::invoke::{self, CommonArgs};
use crate::sdk_build;
use crate::sdk_config::{self, SdkEntry};
use crate::spec_config::{self, Mode};
use crate::spec_gen;

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("gen") => run_gen(&args[1..]),
        // The SDK surface is derived from the linked generate-mode specs,
        // so gating those specs is gating the SDK.
        Some("gate") => crate::spec_gate::run(&args[1..]),
        Some("build") => run_build(&args[1..]),
        Some("publish") => run_publish(&args[1..]),
        _ => {
            eprintln!(
                "soothfast: usage: cargo soothfast sdk gen -p PKG [--check]\n\
                 cargo soothfast sdk gate -p PKG [--base REF]\n\
                 cargo soothfast sdk build -p PKG [--target TRIPLE].. [--bench NAME] \
                 [--debug]\n\
                 cargo soothfast sdk publish -p PKG [--only LANG] [--dry-run] \
                 [--allow-unbundled]"
            );
            2
        }
    }
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
            // Cross-compiling five release binaries is slow; debug is for
            // proving the wiring works.
            "--debug" => release = false,
            _ if common.try_parse(a, &mut it) => {}
            other => {
                eprintln!("soothfast: unknown sdk build arg {other:?}");
                return 2;
            }
        }
    }
    let Some(pkg) = common.pkg.clone() else {
        eprintln!("soothfast: sdk build requires -p PKG");
        return 2;
    };
    match build_binaries(&pkg, &common, &targets, release) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: {e}");
            1
        }
    }
}

fn build_binaries(
    pkg: &str,
    common: &CommonArgs,
    cli_targets: &[String],
    release: bool,
) -> Result<i32, String> {
    let meta = invoke::pkg_meta(pkg).map_err(|e| e.to_string())?;
    let built = build_all(pkg, common, &meta)?;
    let embedding: Vec<&BuiltSdk> = built.iter().filter(|s| s.entry.embed.is_some()).collect();
    if embedding.is_empty() {
        println!("sdk build: nothing to build — no [[sdk]] entry sets `embed`");
        return Ok(0);
    }

    let target_dir = invoke::target_dir(None).map_err(|e| e.to_string())?;
    let mut failed = 0u32;
    for sdk in embedding {
        let binary = sdk.entry.embed.as_deref().unwrap_or_default();
        let requested = if cli_targets.is_empty() {
            sdk.entry.targets.clone()
        } else {
            cli_targets.to_vec()
        };
        let targets = Target::matrix(&requested)?;
        println!(
            "sdk build: {} [{}] — {} target(s) for `{binary}`",
            sdk.entry.out,
            sdk.entry.lang.name(),
            targets.len()
        );

        let (compiled, skipped) = sdk_build::compile(pkg, binary, &targets, release, &target_dir);
        for why in &skipped {
            println!("  SKIP {why}");
        }
        if compiled.is_empty() {
            failed += 1;
            println!("  no target built — nothing staged");
            continue;
        }

        let out_dir = meta.dir.join(&sdk.entry.out);
        match sdk.entry.lang {
            SdkKind::TypeScript => {
                let staged = sdk_build::stage_npm(&out_dir, &sdk.entry.package, binary, &compiled)?;
                for triple in staged {
                    println!("  staged {triple}");
                }
            }
            SdkKind::Python => {
                if let Some(warning) = sdk_build::manylinux_warning(&compiled) {
                    println!("  WARNING: {warning}");
                }
                for tag in sdk_build::build_wheels(&out_dir, &compiled)? {
                    println!("  built {tag}");
                }
            }
        }
        if !skipped.is_empty() {
            println!(
                "  {} of {} target(s) missing — the published package will \
                 not cover them",
                skipped.len(),
                targets.len()
            );
        }
    }
    Ok(if failed > 0 { 1 } else { 0 })
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
    // SDKs render the same operations the spec does, so they resolve types
    // out of the same workspace crates.
    let aux = spec_gen::aux_rustdoc_for(pkg, None, &spec_cfg);
    let docs = Docs::new(&doc, &aux);

    let mut built = Vec::new();
    for entry in sdk_cfg.entries {
        let routes = by_spec
            .get(&entry.spec)
            .ok_or_else(|| format!("no #[route] in {pkg} declares spec {:?}", entry.spec))?;
        let spec_entry = spec_cfg.for_path(&entry.spec);
        let (ops, gaps) = spec_gen::operations_for(docs, spec_entry, routes)?;
        let info = spec_gen::info_for(spec_entry, pkg, meta);
        let env_vars = embed_env_vars(&entry, &meta.dir)?;
        let opts = sdk_options(&entry, meta, env_vars);
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

/// The knobs the embedded server documents, read from its own dotenv
/// template. Parsing the server's file rather than restating it here is
/// what keeps an SDK's configuration surface from drifting from the
/// server's.
fn embed_env_vars(
    entry: &SdkEntry,
    dir: &std::path::Path,
) -> Result<Vec<soothfast_sdk::envtemplate::EnvVar>, String> {
    let Some(rel) = &entry.embed_env_template else {
        return Ok(Vec::new());
    };
    if entry.embed.is_none() {
        return Err(format!(
            "[[sdk]] {} sets `embed_env_template` but no `embed` — there is \
             no bundled server for those knobs to configure",
            entry.package
        ));
    }
    let path = dir.join(rel);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("[[sdk]] embed_env_template {}: {e}", path.display()))?;
    Ok(soothfast_sdk::envtemplate::parse(&text))
}

fn sdk_options(
    entry: &SdkEntry,
    meta: &invoke::PkgMeta,
    env_vars: Vec<soothfast_sdk::envtemplate::EnvVar>,
) -> SdkOptions {
    let defaults = SdkOptions::default();
    SdkOptions {
        package: entry.package.clone(),
        module: entry.module(),
        version: entry
            .version
            .clone()
            .unwrap_or_else(|| meta.version.clone()),
        base_url: entry.base_url.clone(),
        targets: entry.targets.clone(),
        embed: entry.embed.clone(),
        embed_args: entry.embed_args.clone(),
        embed_env: entry.embed_env.clone(),
        embed_env_vars: env_vars,
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
    let mut allow_unbundled = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--only" => only = it.next().cloned(),
            "--dry-run" => dry_run = true,
            "--allow-unbundled" => allow_unbundled = true,
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
    match publish(&pkg, &common, only.as_deref(), dry_run, allow_unbundled) {
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
    allow_unbundled: bool,
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
        let out_dir = meta.dir.join(&sdk.entry.out);
        // An embedding package with nothing staged would resolve its
        // binary off the user's PATH — self-contained in name only.
        let staged = match &sdk.entry.embed {
            Some(binary) => {
                sdk_build::bundled_targets(sdk.entry.lang, &out_dir, &sdk.entry.package, binary)
            }
            None => Vec::new(),
        };
        if let Some(binary) = &sdk.entry.embed {
            let bundled = &staged;
            let wanted = Target::matrix(&sdk.entry.targets)?;
            if bundled.is_empty() && !allow_unbundled {
                return Err(format!(
                    "{}: `embed = {binary:?}` but nothing is staged — run \
                     `cargo soothfast sdk build -p {pkg}` first, or pass \
                     --allow-unbundled to publish a package that falls back \
                     to finding `{binary}` on the user's PATH",
                    sdk.entry.out
                ));
            }
            let missing: Vec<&str> = wanted
                .iter()
                .map(|t| t.triple)
                .filter(|t| !bundled.iter().any(|b| b == t))
                .collect();
            if !missing.is_empty() && !allow_unbundled {
                return Err(format!(
                    "{}: no binary staged for {} — those platforms would \
                     fall back to PATH. Build them, narrow `targets`, or \
                     pass --allow-unbundled",
                    sdk.entry.out,
                    missing.join(", ")
                ));
            }
        }
        if let Some(v) = &sdk.entry.version
            && *v != meta.version
        {
            return Err(format!(
                "{}: [[sdk]] version {v} does not match crate version {} — \
                     SDK releases stay in lockstep with the crate",
                sdk.entry.out, meta.version
            ));
        }
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
        let embedded = sdk.entry.embed.is_some();
        match sdk.entry.lang {
            SdkKind::Python => publish_python(&out_dir, embedded, dry_run)?,
            SdkKind::TypeScript => {
                publish_typescript(&out_dir, &sdk.entry.package, &staged, dry_run)?
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

/// An embedding SDK publishes the platform wheels `sdk build` already
/// produced — rebuilding here would replace them with one untagged wheel
/// carrying no binary.
fn publish_python(out_dir: &Path, embedded: bool, dry_run: bool) -> Result<(), String> {
    if !embedded {
        run_tool("uv", &["build"], out_dir)?;
    } else if !out_dir.join("dist").is_dir() {
        return Err("no dist/ — run `cargo soothfast sdk build` first".into());
    }
    if !dry_run {
        run_tool("uv", &["publish"], out_dir)?;
    }
    Ok(())
}

/// `npm publish` runs the package's own `prepublishOnly` build, so the
/// dry run has to compile too — otherwise it would pass on a package that
/// cannot be built.
///
/// Platform packages go first: the main package's `optionalDependencies`
/// name them by exact version, so publishing it first would leave a window
/// where every install fails to find its binary.
fn publish_typescript(
    out_dir: &Path,
    package: &str,
    staged: &[String],
    dry_run: bool,
) -> Result<(), String> {
    let publish: &[&str] = if dry_run {
        &["publish", "--dry-run"]
    } else {
        &["publish"]
    };

    for triple in staged {
        let Some(target) = Target::find(triple) else {
            continue;
        };
        let dir = out_dir
            .join("platforms")
            .join(format!("{package}-{}", target.npm_suffix()));
        if dir.is_dir() {
            run_tool("npm", publish, &dir)?;
        }
    }

    // The optional deps are not published yet on a first release, and a
    // failed optional install is not fatal — but skipping them keeps the
    // build step quiet and offline.
    run_tool(
        "npm",
        &["install", "--no-audit", "--no-fund", "--omit=optional"],
        out_dir,
    )?;
    run_tool("npm", &["run", "build"], out_dir)?;
    run_tool("npm", publish, out_dir)
}

/// How to get a missing publishing tool.
fn install_hint(tool: &str) -> &'static str {
    match tool {
        "uv" => "curl -LsSf https://astral.sh/uv/install.sh | sh",
        "npm" => "install Node.js 18 or newer — https://nodejs.org",
        _ => "install it and re-run",
    }
}

fn run_tool(tool: &str, args: &[&str], dir: &Path) -> Result<(), String> {
    let status = Command::new(tool)
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("`{tool}` not found — {}", install_hint(tool))
            } else {
                format!("cannot run {tool}: {e}")
            }
        })?;
    if !status.success() {
        return Err(format!("`{tool} {}` failed", args.join(" ")));
    }
    Ok(())
}
