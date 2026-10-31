//! `cargo soothfast docs <check|accept|gen-tests|capture|diff|reference>` —
//! the docs-engine commands, driving `soothfast-docs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use soothfast_docs::{claims, diff, gentests, lockfile, markdown, reference, surface};

use crate::{docs_binds, docs_config, docs_generated, docs_support, glob, invoke};

#[derive(Default)]
struct DocsArgs {
    /// The last `-p`, for commands that act on one package.
    pkg: Option<String>,
    /// Every `-p`, for resolving binds across crates.
    pkgs: Vec<String>,
    baseline: String,
    against_ref: Option<String>,
    out: Option<PathBuf>,
    check_only: bool,
    /// `docs check` leaves claims unevaluated, for a run with no measurement.
    skip_claims: bool,
    features: Option<String>,
    /// `docs routes` `[[bench]]` target to link the route registry from.
    target: Option<String>,
    /// `docs serve` bind address (HOST:PORT).
    addr: Option<String>,
    /// `docs gh-deploy` target branch (default "gh-pages").
    branch: String,
    /// `docs gh-deploy` target remote (default "origin").
    remote: String,
    /// `docs gh-deploy` commit message (default: "docs: publish site for <sha>").
    message: Option<String>,
    paths: Vec<PathBuf>,
}

/// The item of a "no measurement" error, when the baseline lists its package
/// as unmeasured: a claim about it is not evaluated rather than failed.
fn unmeasured_item(error: &str, baseline: &serde_json::Value) -> Option<String> {
    let named = error.strip_prefix("no measurement for ")?;
    let metric_path = named.split(" in baseline").next()?;
    let item = metric_path.rsplitn(3, '.').nth(2)?;
    let pkg = invoke::id_pkg(item);
    baseline["unmeasured"]
        .as_array()?
        .iter()
        .any(|p| p.as_str() == Some(pkg))
        .then(|| item.to_string())
}

pub fn run(args: &[String]) -> i32 {
    let Some(sub) = args.first().cloned() else {
        eprintln!(
            "soothfast: usage: cargo soothfast docs check|accept|gen-tests|capture|diff|reference|routes|build|serve|gh-deploy"
        );
        return 2;
    };
    let mut a = DocsArgs {
        pkg: None,
        pkgs: Vec::new(),
        baseline: "base".into(),
        against_ref: None,
        out: None,
        check_only: false,
        skip_claims: false,
        features: None,
        target: None,
        addr: None,
        branch: "gh-pages".into(),
        remote: "origin".into(),
        message: None,
        paths: Vec::new(),
    };
    let mut it = args[1..].iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-p" | "--package" => {
                a.pkg = it.next().cloned();
                a.pkgs.extend(a.pkg.clone());
            }
            "--baseline" => match it.next() {
                Some(n) => a.baseline = n.clone(),
                None => return err("--baseline needs a name"),
            },
            "--against-ref" => a.against_ref = it.next().cloned(),
            "--out" => a.out = it.next().map(PathBuf::from),
            "--check" => a.check_only = true,
            "--skip-claims" => a.skip_claims = true,
            "--features" => a.features = it.next().cloned(),
            "--target" => a.target = it.next().cloned(),
            "--addr" => match it.next() {
                Some(addr) => a.addr = Some(addr.clone()),
                None => return err("--addr needs HOST:PORT"),
            },
            "--branch" => match it.next() {
                Some(b) => a.branch = b.clone(),
                None => return err("--branch needs a name"),
            },
            "--remote" => match it.next() {
                Some(r) => a.remote = r.clone(),
                None => return err("--remote needs a name"),
            },
            "--message" => match it.next() {
                Some(m) => a.message = Some(m.clone()),
                None => return err("--message needs text"),
            },
            p if !p.starts_with('-') => a.paths.push(PathBuf::from(p)),
            other => return err(&format!("unknown docs arg {other:?}")),
        }
    }

    match sub.as_str() {
        "check" => check(&a),
        "accept" => accept(&a),
        "gen-tests" => gen_tests(&a),
        "capture" => capture(&a),
        "diff" => surface_diff(&a),
        "reference" => reference_cmd(&a),
        // Reject args other subcommands take: silently ignoring `-p` or
        // PATHS would mask a misunderstanding of what `build` builds.
        "build" if a.pkg.is_some() => err("docs build takes no -p (it builds the whole site)"),
        "build" if !a.paths.is_empty() => err("docs build takes no PATH args (scope is docs_dir)"),
        "build" => crate::site::build_cmd(&a.baseline, a.out.as_deref()),
        "serve" if a.pkg.is_some() => err("docs serve takes no -p (it serves the whole site)"),
        "serve" => crate::site::serve_cmd(&a.baseline, a.addr.as_deref()),
        "gh-deploy" if a.pkg.is_some() => {
            err("docs gh-deploy takes no -p (it deploys the whole site)")
        }
        "gh-deploy" if !a.paths.is_empty() => {
            err("docs gh-deploy takes no PATH args (scope is docs_dir)")
        }
        "gh-deploy" => {
            crate::site::gh_deploy_cmd(&a.baseline, &a.branch, &a.remote, a.message.as_deref())
        }
        "routes" => match &a.pkg {
            Some(pkg) => crate::spec::routes_cmd(
                pkg,
                &a.baseline,
                a.out.as_deref(),
                a.features.as_deref(),
                a.target.as_deref(),
            ),
            None => err("routes needs -p PKG"),
        },
        other => err(&format!("unknown docs subcommand {other:?}")),
    }
}

fn err(msg: &str) -> i32 {
    eprintln!("soothfast: {msg}");
    2
}

/// Markdown files in scope: explicit paths (dirs recursed), or the `[docs]
/// paths` of `soothfast.toml` (files, dirs and globs under the project root).
fn md_files(a: &DocsArgs, root: &Path, scope: &[String]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if a.paths.is_empty() {
        for entry in scope {
            if entry.contains(['*', '?']) {
                let matched = tracked_md(root)
                    .into_iter()
                    .filter(|f| glob::matches(entry, f));
                files.extend(matched.map(|f| root.join(f)));
            } else if root.join(entry).exists() {
                collect_md(&root.join(entry), &mut files);
            }
        }
    } else {
        for p in &a.paths {
            collect_md(&root.join(p), &mut files);
        }
    }
    files.sort();
    files.dedup();
    files
}

/// Tracked markdown files, relative to `root`. Empty outside a git checkout.
fn tracked_md(root: &Path) -> Vec<String> {
    invoke::git_in(root, &["ls-files", "*.md"])
        .map(|out| out.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn is_generated(text: &str) -> bool {
    head_for_sniff(text, 400).contains("Generated by `cargo soothfast")
}

/// Tracked markdown outside the scanned scope that carries markers, which no
/// check reads.
fn unscanned_marker_files(scanned: &Scanned) -> Vec<String> {
    let in_scope: BTreeSet<&str> = scanned.docs.iter().map(|(f, _, _)| f.as_str()).collect();
    tracked_md(&scanned.root)
        .into_iter()
        .filter(|f| !in_scope.contains(f.as_str()))
        .filter_map(|f| {
            let text = std::fs::read_to_string(scanned.root.join(&f)).ok()?;
            if is_generated(&text) {
                return None;
            }
            let doc = markdown::scan(&text).ok()?;
            (!doc.binds.is_empty() || !doc.claims.is_empty()).then_some(f)
        })
        .collect()
}

fn collect_md(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for e in entries.flatten() {
                collect_md(&e.path(), out);
            }
        }
    } else if path.extension().is_some_and(|e| e == "md") {
        out.push(path.to_path_buf());
    }
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// First ≤`limit` bytes of `text`, backed off to a char boundary so slicing
/// non-ASCII docs can't panic.
fn head_for_sniff(text: &str, limit: usize) -> &str {
    let cut = (0..=text.len().min(limit))
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    &text[..cut]
}

struct Scanned {
    root: PathBuf,
    /// (relative path, parsed doc, raw text)
    docs: Vec<(String, markdown::Doc, String)>,
}

impl Scanned {
    /// Every file gen-tests generates for the package at `pkg_dir`, and how
    /// many pages beyond those in scope it covers. PATHS assign pages to a
    /// package, so a scoped run also keeps each page the package's generated
    /// files already record; an unscoped run covers exactly the default scope.
    fn generated(
        &self,
        a: &DocsArgs,
        pkg_dir: &Path,
    ) -> Result<(Vec<gentests::Generated>, usize), String> {
        let mut kept = Vec::new();
        if !a.paths.is_empty() {
            let in_scope: BTreeSet<String> = self
                .docs
                .iter()
                .map(|(file, _, _)| docs_generated::page_key(file))
                .collect();
            for page in docs_generated::recorded_pages(pkg_dir) {
                let path = self.root.join(&page);
                if in_scope.contains(&page) || !path.is_file() {
                    continue;
                }
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{page}: {e}"))?;
                let doc = markdown::scan(&text).map_err(|e| format!("{page}: {e}"))?;
                kept.push((page, doc));
            }
        }
        let pages = || {
            self.docs
                .iter()
                .map(|(file, doc, _)| (file.as_str(), doc))
                .chain(kept.iter().map(|(page, doc)| (page.as_str(), doc)))
        };
        let mut files = gentests::doc_test_set(pages())?;
        files.extend(gentests::capture_set(pages())?);
        Ok((files, kept.len()))
    }
}

fn print_kept(kept: usize, pkg: &str) {
    if kept > 0 {
        println!("also kept: {kept} page(s) already generated for {pkg}");
    }
}

fn scan_all(a: &DocsArgs) -> Result<Scanned, String> {
    scan_at(a, invoke::project_root())
}

fn scan_at(a: &DocsArgs, root: PathBuf) -> Result<Scanned, String> {
    let mut docs = Vec::new();
    let scope = docs_config::load(&root)?.paths;
    for file in md_files(a, &root, &scope) {
        let text =
            std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        // Our own generated fragments are outputs, not sources — don't scan.
        if is_generated(&text) {
            continue;
        }
        let relpath = rel(&root, &file);
        let doc = markdown::scan(&text).map_err(|e| format!("{relpath}: {e}"))?;
        docs.push((relpath, doc, text));
    }
    Ok(Scanned { root, docs })
}

/// The rustdoc surface of each `-p` package, built only when some bind names
/// a Rust item.
fn surfaces_for_binds(
    a: &DocsArgs,
    scanned: &Scanned,
) -> Result<Vec<(String, surface::Surface)>, String> {
    let rust_binds = scanned
        .docs
        .iter()
        .flat_map(|(_, d, _)| &d.binds)
        .flat_map(|b| b.targets())
        .any(|t| matches!(docs_binds::classify(t), docs_binds::Target::Rust(_)));
    if !rust_binds {
        return Ok(Vec::new());
    }
    if a.pkgs.is_empty() {
        return Err("this docs command needs -p PKG".into());
    }
    a.pkgs
        .iter()
        .map(|pkg| {
            let (surf, _, _) = build_surface(a, pkg)?;
            Ok((pkg.clone(), surf))
        })
        .collect()
}

fn build_surface(a: &DocsArgs, pkg: &str) -> Result<(surface::Surface, Value, PathBuf), String> {
    let (doc, root) =
        invoke::rustdoc_json_in(pkg, None, a.features.as_deref(), invoke::Visibility::Public)
            .map_err(|e| e.to_string())?;
    Ok((surface::from_rustdoc(&doc, &root), doc, root))
}

fn check(a: &DocsArgs) -> i32 {
    match scan_all(a) {
        Ok(scanned) => check_scanned(a, &scanned),
        Err(e) => err(&e),
    }
}

fn check_scanned(a: &DocsArgs, scanned: &Scanned) -> i32 {
    let mut failures = 0u32;

    // 0. Markers in tracked markdown the scan scope leaves out.
    if a.paths.is_empty() {
        for file in unscanned_marker_files(scanned) {
            failures += 1;
            println!(
                "FAIL  {file}: has soothfast markers outside the scanned scope; pass it as a PATH or list it under `paths` in the [docs] table of soothfast.toml"
            );
        }
    }

    // 1. Bind fingerprints vs soothfast.lock.
    let any_binds = scanned.docs.iter().any(|(_, d, _)| !d.binds.is_empty());
    if any_binds {
        let surfs = match surfaces_for_binds(a, scanned) {
            Ok(s) => s,
            Err(e) => return err(&format!("binds present but surface unavailable: {e}")),
        };
        let lock = match lockfile::read(&scanned.root) {
            Ok(l) => l,
            Err(e) => return err(&format!("cannot read soothfast.lock: {e}")),
        };
        if lock.version != lockfile::VERSION && !lock.binds.is_empty() {
            failures += 1;
            let advice = if lock.version < lockfile::VERSION {
                "re-verify each bind, then `docs accept`"
            } else {
                "upgrade cargo-soothfast to match"
            };
            println!(
                "FAIL  {} is format v{}, this build derives v{}: fingerprints are not comparable across formats, so {advice}",
                lockfile::LOCKFILE,
                lock.version,
                lockfile::VERSION
            );
        } else {
            for (file, doc, _) in &scanned.docs {
                for bind in &doc.binds {
                    for target in bind.targets() {
                        let current = match docs_binds::resolve(&scanned.root, &surfs, target) {
                            Ok(c) => c,
                            Err(e) => {
                                failures += 1;
                                println!("FAIL  {file}:{} bind {target}: {e}", bind.line);
                                continue;
                            }
                        };
                        match lock.binds.get(target) {
                            None => {
                                failures += 1;
                                let pkg: String =
                                    a.pkgs.iter().map(|pkg| format!(" -p {pkg}")).collect();
                                println!(
                                    "FAIL  {file}:{} bind {target}: not locked — verify the prose, then `cargo soothfast docs accept{pkg}`",
                                    bind.line
                                );
                            }
                            Some(locked) if *locked != current => {
                                failures += 1;
                                println!(
                                    "FAIL  {file}:{} bind {target}: STALE — code changed under this prose; re-verify, then `docs accept`",
                                    bind.line
                                );
                            }
                            Some(_) => println!("ok    {file}:{} bind {target}", bind.line),
                        }
                    }
                }
            }
        }
    }

    // 2. Claims vs the measurement baseline.
    let any_claims = scanned.docs.iter().any(|(_, d, _)| !d.claims.is_empty());
    if any_claims && a.skip_claims {
        println!("docs check: claims not evaluated: no measurement");
    } else if any_claims {
        let baseline = match invoke::load_baseline_in(&scanned.root, &a.baseline) {
            Ok(Some(b)) => b,
            Ok(None) => {
                return err(&format!(
                    "claims present but no baseline {:?} — run `cargo soothfast measure --save-baseline {}`",
                    a.baseline, a.baseline
                ));
            }
            Err(e) => return err(&format!("cannot load baseline: {e}")),
        };
        for (file, doc, _) in &scanned.docs {
            for cm in &doc.claims {
                match claims::parse(&cm.expr).and_then(|c| claims::evaluate(&c, &baseline)) {
                    Ok((true, actual)) => {
                        println!(
                            "ok    {file}:{} claim `{}` (actual {actual:.1})",
                            cm.line, cm.expr
                        )
                    }
                    Ok((false, actual)) => {
                        failures += 1;
                        println!(
                            "FAIL  {file}:{} claim `{}` VIOLATED (actual {actual:.1})",
                            cm.line, cm.expr
                        );
                    }
                    Err(e) => match unmeasured_item(&e, &baseline) {
                        Some(item) => println!(
                            "skip  {file}:{} claim `{}` not evaluated: no measurement for {item}",
                            cm.line, cm.expr
                        ),
                        None => {
                            failures += 1;
                            println!("FAIL  {file}:{} claim `{}`: {e}", cm.line, cm.expr);
                        }
                    },
                }
            }
        }
    }

    // 3. Generated-artifact manifest: regenerate in memory, compare to disk.
    if let Some(pkg) = &a.pkg {
        match invoke::pkg_dir(pkg) {
            Err(e) => return err(&e.to_string()),
            Ok(dir) => {
                let (expected, kept) = match scanned.generated(a, &dir) {
                    Ok(g) => g,
                    Err(e) => return err(&e),
                };
                print_kept(kept, pkg);
                let problems = docs_generated::check(&dir, pkg, &expected, None);
                if problems.is_empty() && !expected.is_empty() {
                    println!(
                        "ok    generated tests and capture example up to date ({} file(s))",
                        expected.len()
                    );
                }
                for problem in &problems {
                    failures += 1;
                    println!("FAIL  {problem}");
                }
            }
        }
    }

    if failures > 0 {
        println!("docs check: FAILED ({failures} problem(s))");
        1
    } else {
        println!("docs check: passed ({} file(s))", scanned.docs.len());
        0
    }
}

fn accept(a: &DocsArgs) -> i32 {
    match scan_all(a) {
        Ok(scanned) => accept_scanned(a, &scanned),
        Err(e) => err(&e),
    }
}

fn accept_scanned(a: &DocsArgs, scanned: &Scanned) -> i32 {
    let surfs = match surfaces_for_binds(a, scanned) {
        Ok(s) => s,
        Err(e) => return err(&e),
    };
    let mut fresh = lockfile::Binds::new();
    for (file, doc, _) in &scanned.docs {
        for bind in &doc.binds {
            for target in bind.targets() {
                match docs_binds::resolve(&scanned.root, &surfs, target) {
                    Ok(fp) => fresh.insert(target.to_string(), fp),
                    Err(e) => {
                        return err(&format!("{file}:{} bind {target}: {e}", bind.line));
                    }
                };
            }
        }
    }
    // Explicit PATHS scan a subset of docs: merge so locks for binds in
    // out-of-scope files survive. Full scope replaces (drops dead binds).
    let full_scope = a.paths.is_empty();
    let existing = if full_scope {
        lockfile::Binds::new()
    } else {
        match lockfile::read(&scanned.root).and_then(lockfile::comparable) {
            Ok(binds) => binds,
            Err(e) => return err(&e),
        }
    };
    let binds = lockfile::merge(&existing, &fresh, full_scope);
    match lockfile::write(&scanned.root, &binds) {
        Ok(()) => {
            println!(
                "docs accept: locked {} bind(s) in {}",
                binds.len(),
                lockfile::LOCKFILE
            );
            0
        }
        Err(e) => err(&format!("cannot write lockfile: {e}")),
    }
}

fn gen_tests(a: &DocsArgs) -> i32 {
    let Some(pkg) = a.pkg.clone() else {
        return err("gen-tests needs -p PKG (where tests/examples are written)");
    };
    let scanned = match scan_all(a) {
        Ok(s) => s,
        Err(e) => return err(&e),
    };
    let dir = match invoke::pkg_dir(&pkg) {
        Ok(d) => d,
        Err(e) => return err(&e.to_string()),
    };
    let (expected, kept) = match scanned.generated(a, &dir) {
        Ok(g) => g,
        Err(e) => return err(&e),
    };
    print_kept(kept, &pkg);
    let (wrote, removed) = match docs_generated::write(&dir, &expected) {
        Ok(r) => r,
        Err(e) => return err(&format!("cannot write generated files: {e}")),
    };
    for path in &wrote {
        println!("wrote {}", dir.join(path).display());
    }
    for path in &removed {
        println!("removed {}", dir.join(path).display());
    }
    println!(
        "docs gen-tests: {} written, {} removed, {} unchanged",
        wrote.len(),
        removed.len(),
        expected.len() - wrote.len()
    );
    0
}

fn capture(a: &DocsArgs) -> i32 {
    let Some(pkg) = a.pkg.clone() else {
        return err("capture needs -p PKG");
    };
    let scanned = match scan_all(a) {
        Ok(s) => s,
        Err(e) => return err(&e),
    };
    let pages: Vec<(&String, &String, Vec<String>)> = scanned
        .docs
        .iter()
        .map(|(file, doc, text)| (file, text, gentests::capture_ids(file, doc)))
        .filter(|(_, _, ids)| !ids.is_empty())
        .collect();
    if !pages.is_empty() {
        let dir = match invoke::pkg_dir(&pkg) {
            Ok(d) => d,
            Err(e) => return err(&e.to_string()),
        };
        if let Err(e) = current_capture(a, &scanned, &pkg, &dir) {
            return e;
        }
        if let Err(e) = build_capture(&pkg, a.features.as_deref(), &dir) {
            return e;
        }
    }
    let (mut checked, mut stale, mut skipped, mut failures) = (0u32, 0u32, 0u32, 0u32);
    for (file, text, ids) in pages {
        let mut current = text.clone();
        for (n, id) in ids.iter().enumerate() {
            let mut cmd = capture_cmd("run", &pkg, a.features.as_deref());
            let out = match cmd.args(["--", id]).output() {
                Ok(o) => o,
                Err(e) => return err(&format!("cannot run capture block {id}: {e}")),
            };
            let stdout = match CaptureRun::of(&out) {
                CaptureRun::Output(stdout) => stdout,
                CaptureRun::Disabled => {
                    skipped += 1;
                    println!("skip  {file}: block {id} (features not enabled)");
                    continue;
                }
                CaptureRun::Unknown => {
                    failures += 1;
                    println!(
                        "FAIL  {file}: block {id} is not in the capture example — run `cargo soothfast docs gen-tests -p {pkg}`"
                    );
                    continue;
                }
                CaptureRun::Failed(stderr) => {
                    failures += 1;
                    println!(
                        "FAIL  {file}: block {id} exited with {}\n{stderr}",
                        out.status
                    );
                    continue;
                }
            };
            checked += 1;
            match markdown::splice_output(&current, n, &stdout) {
                Ok(Some(next)) => {
                    if next != current {
                        stale += 1;
                    }
                    current = next;
                }
                Ok(None) => {
                    failures += 1;
                    println!("FAIL  {file}: capture block {n} not found for splicing");
                }
                Err(e) => return err(&format!("{file}: {e}")),
            }
        }
        if current == *text {
            println!("ok    {file}: captured output current");
        } else if a.check_only {
            failures += 1;
            println!(
                "FAIL  {file}: captured output is stale — run `cargo soothfast docs capture -p {pkg}`"
            );
        } else {
            let path = scanned.root.join(file);
            if std::fs::write(&path, &current).is_err() {
                return err(&format!("cannot write {}", path.display()));
            }
            println!("spliced fresh output into {file}");
        }
    }
    println!(
        "docs capture: {checked} checked, {stale} stale, {skipped} skipped (features not enabled)"
    );
    if failures > 0 {
        println!("docs capture: FAILED ({failures})");
        1
    } else {
        0
    }
}

/// Fail unless the capture example on disk is what gen-tests would write now.
/// Block ids are positions, so a stale example would run another block's code.
fn current_capture(a: &DocsArgs, scanned: &Scanned, pkg: &str, dir: &Path) -> Result<(), i32> {
    let (mut expected, _) = scanned.generated(a, dir).map_err(|e| err(&e))?;
    let capture = Some(gentests::CAPTURE_DIR);
    expected.retain(|g| gentests::owning_target(&g.path).map(|(t, _)| t) == capture);
    let problems = docs_generated::check(dir, pkg, &expected, capture);
    if problems.is_empty() {
        return Ok(());
    }
    for problem in &problems {
        println!("FAIL  {problem}");
    }
    println!("docs capture: FAILED (generated capture example is stale, so no block ran)");
    Err(1)
}

/// Build the package's capture example once, so a compile error is reported
/// as one failure naming the blocks it is in, not as every block failing.
fn build_capture(pkg: &str, features: Option<&str>, dir: &Path) -> Result<(), i32> {
    let out = capture_cmd("build", pkg, features)
        .arg("--message-format=json")
        .output()
        .map_err(|e| err(&format!("cannot build the capture example: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let diagnostics: Vec<Value> = invoke::cargo_messages(&out.stdout)
        .filter(|m| m["reason"] == "compiler-message")
        .map(|m| m["message"].clone())
        .collect();
    println!(
        "FAIL  {}: the shared capture example does not compile, so no block ran",
        gentests::CAPTURE_DIR
    );
    for block in blocks_named_in(&diagnostics) {
        let Ok(text) = std::fs::read_to_string(dir.join(gentests::CAPTURE_DIR).join(&block)) else {
            continue;
        };
        match gentests::recorded_source(&text) {
            Some((page, Some(line))) => println!("      error in {page} line {line} ({block})"),
            Some((page, None)) => println!("      error in {page} ({block})"),
            None => {}
        }
    }
    for rendered in diagnostics.iter().filter_map(|d| d["rendered"].as_str()) {
        print!("{rendered}");
    }
    print!("{}", String::from_utf8_lossy(&out.stderr));
    println!("docs capture: FAILED (capture example does not compile)");
    Err(1)
}

/// `cargo VERB -q -p PKG --example soothfast_capture [--features F]`, one
/// spelling for the build and every block's run, so each block runs that build.
fn capture_cmd(verb: &str, pkg: &str, features: Option<&str>) -> std::process::Command {
    let mut cmd = std::process::Command::new("cargo");
    cmd.args([
        verb,
        "-q",
        "-p",
        pkg,
        "--example",
        gentests::CAPTURE_EXAMPLE,
    ]);
    if let Some(features) = features {
        cmd.args(["--features", features]);
    }
    cmd
}

/// The `block_<id>.rs` files compiler diagnostics point into, in order.
fn blocks_named_in(diagnostics: &[Value]) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let files = diagnostics
        .iter()
        .filter_map(|d| d["spans"].as_array())
        .flatten()
        .filter_map(|span| span["file_name"].as_str());
    for file in files {
        let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
        if name.starts_with("block_") && name.ends_with(".rs") && !blocks.iter().any(|b| b == name)
        {
            blocks.push(name.to_string());
        }
    }
    blocks
}

/// How one run of the capture example for a block ended.
#[derive(Debug, PartialEq)]
enum CaptureRun {
    Output(String),
    /// The block's `feature=` tag is off in this build: keep what is recorded.
    Disabled,
    /// The example predates the block: gen-tests is stale.
    Unknown,
    Failed(String),
}

impl CaptureRun {
    fn of(out: &std::process::Output) -> CaptureRun {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let reported = |(code, sentinel): (i32, &str)| {
            out.status.code() == Some(code) && stderr.lines().any(|l| l.starts_with(sentinel))
        };
        if out.status.success() {
            CaptureRun::Output(String::from_utf8_lossy(&out.stdout).into_owned())
        } else if reported(gentests::CAPTURE_DISABLED) {
            CaptureRun::Disabled
        } else if reported(gentests::CAPTURE_UNKNOWN) {
            CaptureRun::Unknown
        } else {
            CaptureRun::Failed(stderr.into_owned())
        }
    }
}

fn surface_diff(a: &DocsArgs) -> i32 {
    let Some(pkg) = a.pkg.clone() else {
        return err("diff needs -p PKG");
    };
    let Some(refname) = a.against_ref.clone() else {
        return err("diff needs --against-ref REF");
    };
    let (new_surf, _, _) = match build_surface(a, &pkg) {
        Ok(s) => s,
        Err(e) => return err(&e),
    };

    let sha = match invoke::git(&["merge-base", "HEAD", &refname]) {
        Ok(s) => s,
        Err(e) => return err(&e.to_string()),
    };
    let old_surf = match invoke::with_merge_base_worktree(&refname, |wt| {
        invoke::rustdoc_json_in(
            &pkg,
            Some(wt),
            a.features.as_deref(),
            invoke::Visibility::Public,
        )
        .map(|(doc, wt_root)| surface::from_rustdoc(&doc, &wt_root))
        .map_err(|e| format!("cannot build surface at {sha}: {e}"))
    }) {
        Ok(s) => s,
        Err(e) => return err(&e),
    };

    let d = diff::compare(&old_surf, &new_surf);
    print!("{}", diff::render(&d));
    println!(
        "surface diff vs {sha}: +{} -{} ~{}",
        d.added.len(),
        d.removed.len(),
        d.changed.len()
    );
    0
}

fn reference_cmd(a: &DocsArgs) -> i32 {
    let Some(pkg) = a.pkg.clone() else {
        return err("reference needs -p PKG");
    };
    let (surf, doc, root) = match build_surface(a, &pkg) {
        Ok(s) => s,
        Err(e) => return err(&e),
    };
    // Baseline is optional enrichment: no measurements → plain reference.
    let baseline = invoke::load_baseline(&a.baseline)
        .ok()
        .flatten()
        .unwrap_or(serde_json::Value::Null);
    let text = reference::render(&pkg, &surf, &docs_support::docs_text(&doc), &baseline);
    let out_dir = a.out.clone().unwrap_or_else(|| root.join("docs/reference"));
    let _ = std::fs::create_dir_all(&out_dir);
    let path = out_dir.join(format!("{pkg}.md"));
    match std::fs::write(&path, text) {
        Ok(()) => {
            println!("docs reference: wrote {}", path.display());
            0
        }
        Err(e) => err(&format!("cannot write {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_claim_about_an_unmeasured_package_is_not_evaluated_and_others_still_fail() {
        let baseline = serde_json::json!({ "items": {}, "unmeasured": ["core"] });
        let missing = |item: &str| {
            format!("no measurement for {item}.walltime.median_ns in baseline — run `x`")
        };
        assert_eq!(
            unmeasured_item(&missing("core::sort"), &baseline).as_deref(),
            Some("core::sort")
        );
        assert_eq!(unmeasured_item(&missing("server::route"), &baseline), None);
        assert_eq!(unmeasured_item("claim is malformed", &baseline), None);
    }

    use super::{
        DocsArgs, accept_scanned, blocks_named_in, check_scanned, head_for_sniff, scan_at,
        unmeasured_item, unscanned_marker_files,
    };
    use crate::{docs_binds, invoke};

    #[test]
    fn compiler_errors_name_each_block_file_once() {
        let diagnostics = [
            serde_json::json!({ "spans": [
                { "file_name": "p/examples/soothfast_capture/block_docs_c_1.rs" },
                { "file_name": "src/lib.rs" },
            ] }),
            serde_json::json!({ "spans": [
                { "file_name": "examples/soothfast_capture/block_readme_2.rs" },
                { "file_name": "p\\examples\\soothfast_capture\\block_docs_c_1.rs" },
            ] }),
        ];
        assert_eq!(
            blocks_named_in(&diagnostics),
            ["block_docs_c_1.rs", "block_readme_2.rs"]
        );
    }

    #[test]
    fn head_for_sniff_backs_off_to_char_boundary() {
        // '€' is 3 bytes: byte 400 falls mid-char (the old slice panicked).
        let text = "€".repeat(150);
        let head = head_for_sniff(&text, 400);
        assert_eq!(head, "€".repeat(133));
        assert_eq!(head_for_sniff("short", 400), "short");
        assert_eq!(head_for_sniff("abcdef", 3), "abc");
    }

    const PY: &str = "def parse(s):\n    return s.strip()\n";

    fn project_without_cargo(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("soothfast-nocargo-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/util.py"), PY).unwrap();
        std::fs::write(
            dir.join("README.md"),
            "# Demo\n\n<!-- soothfast:bind src/util.py#parse -->\nparse trims.\n<!-- /soothfast:bind -->\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn file_binds_check_and_accept_without_a_cargo_workspace() {
        let dir = project_without_cargo("roundtrip");
        let a = DocsArgs::default();
        let run =
            |f: fn(&DocsArgs, &super::Scanned) -> i32| f(&a, &scan_at(&a, dir.clone()).unwrap());
        assert_eq!(run(check_scanned), 1, "unlocked bind fails");
        assert_eq!(run(accept_scanned), 0);
        assert_eq!(run(check_scanned), 0);

        std::fs::write(dir.join("src/util.py"), "# note\n".to_string() + PY).unwrap();
        assert_eq!(run(check_scanned), 0, "comment edit stays locked");

        std::fs::write(dir.join("src/util.py"), PY.replace("strip", "lstrip")).unwrap();
        assert_eq!(run(check_scanned), 1, "body edit is stale");
        assert_eq!(run(accept_scanned), 0);
        assert_eq!(run(check_scanned), 0);
    }

    #[test]
    fn claims_read_the_baseline_under_the_project_root() {
        let dir = project_without_cargo("claims");
        std::fs::create_dir_all(dir.join(".soothfast/baselines")).unwrap();
        std::fs::write(
            dir.join(".soothfast/baselines/base.json"),
            r#"{"version":1,"items":{"pytest::t.py::test_parse":{"walltime":{"median_ns":800.0}}}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("README.md"),
            "# Demo\n\n<!-- soothfast:claim pytest::t.py::test_parse.walltime.median_ns < 1000 -->\nfast.\n",
        )
        .unwrap();
        let a = DocsArgs {
            baseline: "base".into(),
            ..DocsArgs::default()
        };
        assert_eq!(check_scanned(&a, &scan_at(&a, dir).unwrap()), 0);
    }

    fn tracked_project(tag: &str) -> std::path::PathBuf {
        let dir = project_without_cargo(tag);
        std::fs::create_dir_all(dir.join("book")).unwrap();
        std::fs::write(dir.join("README.md"), "# Demo\n").unwrap();
        std::fs::write(
            dir.join("book/a.md"),
            "<!-- soothfast:bind src/util.py#parse -->\nparse trims.\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("book/gen.md"),
            "> Generated by `cargo soothfast docs routes`\n<!-- soothfast:bind src/util.py#parse -->\n",
        )
        .unwrap();
        invoke::git_in(&dir, &["init", "-q"]).unwrap();
        invoke::git_in(&dir, &["add", "-A"]).unwrap();
        dir
    }

    #[test]
    fn check_fails_on_markers_outside_the_default_scope() {
        let dir = tracked_project("outside");
        let a = DocsArgs::default();
        let scanned = scan_at(&a, dir).unwrap();
        assert_eq!(unscanned_marker_files(&scanned), ["book/a.md"]);
        assert_eq!(check_scanned(&a, &scanned), 1);
    }

    #[test]
    fn docs_paths_in_the_config_bring_a_directory_into_scope() {
        let dir = tracked_project("config");
        std::fs::write(
            dir.join("soothfast.toml"),
            "[docs]\npaths = [\"README.md\", \"book\"]\n",
        )
        .unwrap();
        let a = DocsArgs::default();
        let scanned = scan_at(&a, dir).unwrap();
        assert!(unscanned_marker_files(&scanned).is_empty());
        assert_eq!(scanned.docs.len(), 2);
    }

    #[test]
    fn docs_paths_accept_globs() {
        let dir = tracked_project("glob");
        std::fs::write(
            dir.join("soothfast.toml"),
            "[docs]\npaths = [\"**/*.md\"]\n",
        )
        .unwrap();
        let a = DocsArgs::default();
        let scanned = scan_at(&a, dir).unwrap();
        let files: Vec<&str> = scanned.docs.iter().map(|(f, _, _)| f.as_str()).collect();
        assert_eq!(files, ["README.md", "book/a.md"]);
    }

    #[test]
    fn explicit_paths_skip_the_outside_scope_check() {
        let dir = tracked_project("explicit");
        let a = DocsArgs {
            paths: vec!["README.md".into()],
            ..DocsArgs::default()
        };
        assert_eq!(check_scanned(&a, &scan_at(&a, dir).unwrap()), 0);
    }

    #[test]
    fn skipped_claims_pass_without_a_baseline() {
        let dir = project_without_cargo("skip");
        std::fs::write(
            dir.join("README.md"),
            "# Demo\n\n<!-- soothfast:claim pytest::t.py::test_parse.walltime.median_ns < 1000 -->\nfast.\n",
        )
        .unwrap();
        let a = DocsArgs::default();
        assert_eq!(check_scanned(&a, &scan_at(&a, dir.clone()).unwrap()), 2);
        let skip = DocsArgs {
            skip_claims: true,
            ..DocsArgs::default()
        };
        assert_eq!(check_scanned(&skip, &scan_at(&skip, dir).unwrap()), 0);
    }

    #[test]
    fn accept_refuses_a_bind_whose_symbol_is_gone() {
        let dir = project_without_cargo("gone");
        std::fs::write(dir.join("src/util.py"), "x = 1\n").unwrap();
        let a = DocsArgs::default();
        assert_eq!(accept_scanned(&a, &scan_at(&a, dir).unwrap()), 2);
    }

    #[test]
    fn a_multi_target_bind_goes_stale_when_any_target_changes() {
        let dir = project_without_cargo("multi");
        std::fs::write(dir.join("src/other.py"), "def other():\n    return 1\n").unwrap();
        std::fs::write(
            dir.join("README.md"),
            "<!-- soothfast:bind src/util.py#parse src/other.py#other -->\nboth.\n<!-- /soothfast:bind -->\n",
        )
        .unwrap();
        let a = DocsArgs::default();
        let run =
            |f: fn(&DocsArgs, &super::Scanned) -> i32| f(&a, &scan_at(&a, dir.clone()).unwrap());
        assert_eq!(run(accept_scanned), 0);
        let lock = soothfast_docs::lockfile::read(&dir).unwrap().binds;
        assert_eq!(
            lock.keys().map(String::as_str).collect::<Vec<_>>(),
            ["src/other.py#other", "src/util.py#parse"]
        );
        assert_eq!(run(check_scanned), 0);

        std::fs::write(dir.join("src/other.py"), "def other():\n    return 2\n").unwrap();
        assert_eq!(run(check_scanned), 1);
        assert_eq!(run(accept_scanned), 0);
        assert_eq!(run(check_scanned), 0);
    }

    #[test]
    fn a_file_only_repo_needs_no_rust_surface() {
        let dir = project_without_cargo("surface");
        let a = DocsArgs::default();
        let scanned = scan_at(&a, dir).unwrap();
        assert!(super::surfaces_for_binds(&a, &scanned).unwrap().is_empty());
        assert_eq!(
            docs_binds::classify(&scanned.docs[0].1.binds[0].item),
            docs_binds::Target::File {
                path: "src/util.py",
                symbol: "parse"
            }
        );
    }
}
