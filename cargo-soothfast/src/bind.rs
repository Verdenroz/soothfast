//! `cargo soothfast bind` — native language bindings from the code.
//!
//! The SDK family's sibling for surfaces with no wire boundary. The exported
//! surface is walked once and lowered per language, so a class defined once
//! in Rust reaches every configured language as the same class.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use soothfast_bind::foreign::TypeTable;
use soothfast_bind::model::Surface;
use soothfast_bind::{BindFileSet, BindKind, BindOptions, compat};

use crate::bind_bench;
use crate::bind_bench_launch;
use crate::bind_config::{self, BindEntry};
use crate::invoke::{self, CommonArgs, ItemMetrics, Run};
use crate::spec_gen;

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("gen") => run_gen(&args[1..]),
        Some("gate") => run_gate(&args[1..]),
        Some("build") => run_build(&args[1..]),
        Some("bench") => run_bench(&args[1..]),
        _ => {
            eprintln!(
                "soothfast: usage: cargo soothfast bind gen -p PKG [--check]\n\
                 cargo soothfast bind gate -p PKG [--base REF] [--allow-breaking]\n\
                 cargo soothfast bind build -p PKG [--target TRIPLE].. [--only LANG[,LANG]] [--debug]\n\
                 cargo soothfast bind bench -p PKG [--only LANG[,LANG]] [--save-baseline NAME] [--json]"
            );
            2
        }
    }
}

/// Parse `--only python,node` into the languages to keep.
fn parse_only(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether `entry` is one `--only` was given, keeps.
fn entry_selected(entry: &BindEntry, only: Option<&[String]>) -> bool {
    only.is_none_or(|langs| langs.iter().any(|l| l == entry.lang.name()))
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
    // A package added by this very branch has no surface at the merge base,
    // and asking cargo about one it has never heard of fails in a way that
    // reads like a broken bench target.
    if !package_dir_in(pkg, wt)?.is_dir() {
        return Ok((Surface::default(), Vec::new()));
    }
    let records = spec_gen::discover_exports(common, Some(wt))?;
    if records.is_empty() {
        return Ok((Surface::default(), Vec::new()));
    }
    let doc = spec_gen::rustdoc_for(pkg, common, Some(wt))?;
    soothfast_bind::walk::surface(&doc, &TypeTable::with_defaults(), &records)
}

/// Where the package sits inside a worktree, by the path it holds in this
/// one.
fn package_dir_in(pkg: &str, wt: &Path) -> Result<PathBuf, String> {
    let meta = invoke::pkg_meta(pkg).map_err(|e| e.to_string())?;
    let root = invoke::workspace_root().map_err(|e| e.to_string())?;
    let relative = meta
        .dir
        .strip_prefix(&root)
        .map_err(|_| format!("{} is outside the workspace", meta.dir.display()))?;
    Ok(wt.join(relative))
}

fn run_build(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut targets: Vec<String> = Vec::new();
    let mut only: Option<String> = None;
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
            "--only" => match it.next() {
                Some(v) => only = Some(v.clone()),
                None => {
                    eprintln!("soothfast: --only needs a language list");
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
    let only = only.as_deref().map(parse_only);
    match build(&pkg, &targets, release, only.as_deref()) {
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

/// The directory holding a bound package's own `Cargo.toml`, relative to
/// `out_dir`. Every backend emits exactly one; R's and Ruby's nest theirs
/// under `src/rust` or `ext/<module>` instead of the output root.
fn manifest_dir(out_dir: &Path, files: &BTreeMap<String, String>) -> Option<PathBuf> {
    let rel = files.keys().find(|k| k.ends_with("Cargo.toml"))?;
    match Path::new(rel).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => Some(out_dir.join(parent)),
        _ => Some(out_dir.to_path_buf()),
    }
}

/// `cargo generate-lockfile` reads the registry index only, so this never
/// needs the target language's own toolchain on the machine.
fn generate_lockfile(dir: &Path) -> Result<(), String> {
    let out = crate::invoke::cargo_command()
        .arg("generate-lockfile")
        .current_dir(dir)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "cargo generate-lockfile failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Whether the manifest in `dir` has no lockfile, or its lockfile no longer
/// satisfies the manifest. `--locked` alone is the guard: it errors only
/// when the lockfile would need to change, so this may still reach the
/// network the same way `generate_lockfile` does on a cold registry cache.
fn lockfile_stale(dir: &Path) -> bool {
    !crate::invoke::cargo_command()
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(dir)
        .output()
        .is_ok_and(|out| out.status.success())
}

/// `cargo update --workspace` refreshes an existing lockfile to satisfy a
/// changed manifest without bumping unrelated dependencies.
fn update_lockfile(dir: &Path) -> Result<(), String> {
    let out = crate::invoke::cargo_command()
        .args(["update", "--workspace"])
        .current_dir(dir)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "cargo update --workspace failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Creates a missing lockfile or refreshes one that no longer satisfies its
/// manifest; a fresh lockfile is left untouched.
fn ensure_lockfile(dir: &Path) -> Result<(), String> {
    if !dir.join("Cargo.lock").exists() {
        generate_lockfile(dir)
    } else if lockfile_stale(dir) {
        update_lockfile(dir)
    } else {
        Ok(())
    }
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
        if let Some(dir) = manifest_dir(&out_dir, &bound.files.files) {
            if check_only {
                if lockfile_stale(&dir) {
                    stale += 1;
                    println!(
                        "STALE {}/Cargo.lock: not locked — run \
                         `cargo soothfast bind gen -p {pkg}` and commit the result",
                        bound.entry.out
                    );
                }
            } else {
                ensure_lockfile(&dir)?;
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

fn build(
    pkg: &str,
    targets: &[String],
    release: bool,
    only: Option<&[String]>,
) -> Result<i32, String> {
    let meta = invoke::pkg_meta(pkg).map_err(|e| e.to_string())?;
    let cfg = bind_config::load(&meta.dir)?;
    if cfg.entries.is_empty() {
        println!("bind build: nothing to build — no [[bind]] entry in soothfast.toml");
        return Ok(0);
    }
    let mut failed = 0u32;
    for entry in cfg.entries.iter().filter(|e| entry_selected(e, only)) {
        let glue = meta.dir.join(&entry.out);
        let wanted = match targets.is_empty() {
            true => entry.targets.clone(),
            false => targets.to_vec(),
        };
        match crate::bind_build::run(entry.lang, &glue, &wanted, release, false) {
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

fn run_bench(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut only: Option<String> = None;
    let mut save_baseline: Option<String> = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--only" => match it.next() {
                Some(v) => only = Some(v.clone()),
                None => {
                    eprintln!("soothfast: --only needs a language list");
                    return 2;
                }
            },
            "--save-baseline" => match it.next() {
                Some(n) => save_baseline = Some(n.clone()),
                None => {
                    eprintln!("soothfast: --save-baseline needs a name");
                    return 2;
                }
            },
            "--json" => json = true,
            _ if common.try_parse(a, &mut it) => {}
            _ => {
                eprintln!("soothfast: unknown bind bench arg {a:?}");
                return 2;
            }
        }
    }
    let Some(pkg) = common.pkg.clone() else {
        eprintln!("soothfast: bind bench requires -p PKG");
        return 2;
    };
    let only = only.as_deref().map(parse_only);
    match bench(&pkg, only.as_deref(), save_baseline.as_deref(), json) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: {e}");
            1
        }
    }
}

/// One shape's measurement, ready to print or file into a baseline.
struct Row {
    lang: &'static str,
    record: bind_bench::BenchRecord,
    ratio: f64,
}

fn bench(
    pkg: &str,
    only: Option<&[String]>,
    save_baseline: Option<&str>,
    json: bool,
) -> Result<i32, String> {
    let meta = invoke::pkg_meta(pkg).map_err(|e| e.to_string())?;
    let cfg = bind_config::load(&meta.dir)?;
    let candidates = bench_candidates(&cfg, only);
    if candidates.is_empty() {
        println!("bind bench: nothing to bench — no [[bind]] entry has a `bench` script");
        return Ok(0);
    }

    let (rows, run, failures) = run_bench_candidates(pkg, &meta.dir, candidates);

    print_rows(&rows, json);
    println!("bind bench: {} shape(s) measured", rows.len());
    for f in &failures {
        println!("bind bench: FAILED {f}");
    }

    save_bench_baseline(save_baseline, &run)?;
    Ok(if failures.is_empty() { 0 } else { 1 })
}

/// Entries configured with a `bench` script, filtered by `--only`.
fn bench_candidates<'a>(
    cfg: &'a bind_config::BindConfig,
    only: Option<&[String]>,
) -> Vec<(&'a BindEntry, &'a String)> {
    cfg.entries
        .iter()
        .filter(|e| entry_selected(e, only))
        .filter_map(|e| e.bench.as_ref().map(|bench| (e, bench)))
        .collect()
}

/// Builds, launches, and measures every candidate, filing each measured
/// shape into the returned `Run` under its baseline item id.
fn run_bench_candidates(
    pkg: &str,
    dir: &Path,
    candidates: Vec<(&BindEntry, &String)>,
) -> (Vec<Row>, Run, Vec<String>) {
    let mut rows = Vec::new();
    let mut run = Run::default();
    let mut failures = Vec::new();
    for (entry, bench_rel) in candidates {
        match bench_entry(entry, bench_rel, dir) {
            Ok(Some(measured)) => {
                for row in measured {
                    let id = bind_bench::item_id(pkg, row.lang, &row.record.shape);
                    run.items.insert(
                        id,
                        ItemMetrics {
                            ratio: Some(row.ratio),
                            binding_ns: Some(row.record.binding_ns),
                            host_ns: Some(row.record.host_ns),
                            n: Some(row.record.n),
                            ..Default::default()
                        },
                    );
                    rows.push(row);
                }
            }
            Ok(None) => {}
            Err(msg) => failures.push(msg),
        }
    }
    (rows, run, failures)
}

/// One entry's build, launch, and measurement. `Ok(None)` covers every skip
/// (missing tool, missing script) — the skip reason is already printed.
fn bench_entry(entry: &BindEntry, bench_rel: &str, dir: &Path) -> Result<Option<Vec<Row>>, String> {
    let glue = dir.join(&entry.out);
    let script = dir.join(bench_rel);
    let label = format!("{} [{}]", entry.out, entry.lang.name());

    if let Some((tool, hint)) = bind_bench_launch::missing_tool(entry.lang) {
        println!("bind bench: skipping {label}: `{tool}` not found — {hint}");
        return Ok(None);
    }
    let artifacts = build_for_bench(entry.lang, &glue, &entry.targets, &label)?;
    let cmd = match bind_bench_launch::launch(entry.lang, &glue, &script, &artifacts) {
        Ok(cmd) => cmd,
        Err(bind_bench_launch::LaunchError::Missing(msg)) => {
            println!("bind bench: skipping {label}: {msg}");
            return Ok(None);
        }
        Err(bind_bench_launch::LaunchError::Failed(msg)) => return Err(format!("{label}: {msg}")),
    };
    let Some(records) = run_script(cmd, &label)? else {
        return Ok(None);
    };
    Ok(Some(
        records
            .into_iter()
            .map(|record| {
                let ratio = bind_bench::ratio(record.binding_ns, record.host_ns);
                Row {
                    lang: entry.lang.name(),
                    record,
                    ratio,
                }
            })
            .collect(),
    ))
}

/// `bind bench` builds its own target rather than requiring a separate
/// `bind build` first: a script otherwise fails with a confusing "cannot
/// find module" instead of a clear build error. Quiet, so a build tool's
/// own stdout never lands in this command's table/JSON output.
fn build_for_bench(
    lang: BindKind,
    glue: &Path,
    targets: &[String],
    label: &str,
) -> Result<Vec<String>, String> {
    crate::bind_build::run(lang, glue, targets, true, true)
        .map_err(|e| format!("{label}: build failed: {e}"))
}

fn save_bench_baseline(name: Option<&str>, run: &Run) -> Result<(), String> {
    let Some(name) = name else {
        return Ok(());
    };
    if run.items.is_empty() {
        println!("bind bench: nothing measured, not saving baseline {name:?}");
        return Ok(());
    }
    let path = invoke::save_baseline(name, run, invoke::SaveScope::BenchFiltered)
        .map_err(|e| e.to_string())?;
    println!("baseline saved: {}", path.display());
    Ok(())
}

/// Run one entry's launch command to completion and parse its stdout.
/// `Ok(None)` means the tool vanished between `launch` building the command
/// and actually spawning it — rare, but the same "skip, don't fail" verdict
/// a `Missing` from `launch` gets.
fn run_script(
    mut cmd: Command,
    label: &str,
) -> Result<Option<Vec<bind_bench::BenchRecord>>, String> {
    let out = match cmd.output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "bind bench: skipping {label}: `{}` not found",
                cmd.get_program().to_string_lossy()
            );
            return Ok(None);
        }
        Err(e) => return Err(format!("{label}: cannot run bench script: {e}")),
    };
    if !out.status.success() {
        return Err(format!(
            "{label} bench script exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    bind_bench::parse_output(&stdout)
        .map(Some)
        .map_err(|e| format!("{label}: {e}"))
}

fn print_rows(rows: &[Row], json: bool) {
    for row in rows {
        if json {
            println!(
                "{{\"lang\":\"{}\",\"shape\":\"{}\",\"binding_ns\":{},\"host_ns\":{},\"n\":{},\"ratio\":{}}}",
                row.lang,
                row.record.shape,
                row.record.binding_ns,
                row.record.host_ns,
                row.record.n,
                row.ratio
            );
        } else {
            println!(
                "{:<8} {:<20} binding={:>12.1}ns  host={:>12.1}ns  ratio={:.2}x",
                row.lang, row.record.shape, row.record.binding_ns, row.record.host_ns, row.ratio
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(lang: &str) -> BindEntry {
        bind_config::parse(&format!(
            "[[bind]]\nlang = \"{lang}\"\nout = \"o\"\npackage = \"p\"\n"
        ))
        .expect("parses")
        .entries
        .remove(0)
    }

    #[test]
    fn parse_only_splits_and_trims() {
        assert_eq!(parse_only("python, node"), vec!["python", "node"]);
        assert_eq!(parse_only("python,,node,"), vec!["python", "node"]);
        assert_eq!(parse_only(""), Vec::<String>::new());
    }

    #[test]
    fn no_only_selects_every_entry() {
        assert!(entry_selected(&entry("python"), None));
        assert!(entry_selected(&entry("go"), None));
    }

    #[test]
    fn only_keeps_just_the_named_languages() {
        let wanted = parse_only("python,node");
        assert!(entry_selected(&entry("python"), Some(&wanted)));
        assert!(entry_selected(&entry("node"), Some(&wanted)));
        assert!(!entry_selected(&entry("go"), Some(&wanted)));
    }

    #[test]
    fn manifest_dir_is_the_output_root_when_cargo_toml_lives_there() {
        let files = BTreeMap::from([("Cargo.toml".to_string(), String::new())]);
        assert_eq!(
            manifest_dir(Path::new("/out"), &files),
            Some(PathBuf::from("/out"))
        );
    }

    #[test]
    fn manifest_dir_follows_a_nested_cargo_toml() {
        let files = BTreeMap::from([("ext/acme_core/Cargo.toml".to_string(), String::new())]);
        assert_eq!(
            manifest_dir(Path::new("/out"), &files),
            Some(PathBuf::from("/out/ext/acme_core"))
        );
    }

    #[test]
    fn manifest_dir_is_none_without_a_cargo_toml() {
        let files = BTreeMap::from([("README.md".to_string(), String::new())]);
        assert_eq!(manifest_dir(Path::new("/out"), &files), None);
    }

    fn write_crate(dir: &Path, name: &str, deps: &str) {
        std::fs::create_dir_all(dir.join("src")).expect("makes dirs");
        std::fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                 [dependencies]\n{deps}"
            ),
        )
        .expect("writes manifest");
        std::fs::write(dir.join("src/lib.rs"), "").expect("writes src/lib.rs");
    }

    #[test]
    fn ensure_lockfile_creates_refreshes_and_leaves_fresh_alone() {
        let root = std::env::temp_dir().join(format!(
            "soothfast-bind-ensure-lockfile-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        write_crate(&root.join("a"), "a", "");
        write_crate(&root.join("c"), "c", "");
        let b = root.join("b");
        write_crate(&b, "b", "a = { path = \"../a\" }\n");

        assert!(lockfile_stale(&b), "no Cargo.lock yet");
        ensure_lockfile(&b).expect("creates a lockfile");
        assert!(
            !lockfile_stale(&b),
            "a fresh lockfile satisfies its manifest"
        );

        let fresh = std::fs::read_to_string(b.join("Cargo.lock")).expect("reads lockfile");
        ensure_lockfile(&b).expect("no-op on a fresh lockfile");
        assert_eq!(
            std::fs::read_to_string(b.join("Cargo.lock")).expect("reads lockfile"),
            fresh,
            "a fresh lockfile is left untouched"
        );

        write_crate(
            &b,
            "b",
            "a = { path = \"../a\" }\nc = { path = \"../c\" }\n",
        );
        assert!(lockfile_stale(&b), "the lock predates the new dependency");
        ensure_lockfile(&b).expect("refreshes a stale lockfile");
        assert!(
            !lockfile_stale(&b),
            "the refreshed lock satisfies the manifest"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
