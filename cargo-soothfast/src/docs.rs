//! `cargo soothfast docs <check|accept|gen-tests|capture|diff|reference>` —
//! the docs-engine commands, driving `soothfast-docs`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use soothfast_docs::{claims, diff, gentests, lockfile, markdown, reference, surface};

use crate::invoke::CommonArgs;
use crate::{docs_support, invoke};

struct DocsArgs {
    pkg: Option<String>,
    baseline: String,
    against_ref: Option<String>,
    out: Option<PathBuf>,
    check_only: bool,
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

pub fn run(args: &[String]) -> i32 {
    let Some(sub) = args.first().cloned() else {
        eprintln!(
            "soothfast: usage: cargo soothfast docs check|accept|gen-tests|capture|diff|reference|routes|build|serve|gh-deploy"
        );
        return 2;
    };
    let mut a = DocsArgs {
        pkg: None,
        baseline: "base".into(),
        against_ref: None,
        out: None,
        check_only: false,
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
            "-p" | "--package" => a.pkg = it.next().cloned(),
            "--baseline" => match it.next() {
                Some(n) => a.baseline = n.clone(),
                None => return err("--baseline needs a name"),
            },
            "--against-ref" => a.against_ref = it.next().cloned(),
            "--out" => a.out = it.next().map(PathBuf::from),
            "--check" => a.check_only = true,
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

/// Markdown files in scope: explicit paths (dirs recursed), or the default
/// README.md + docs/ under the workspace root.
fn md_files(a: &DocsArgs, root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let roots: Vec<PathBuf> = if a.paths.is_empty() {
        let mut v = Vec::new();
        if root.join("README.md").exists() {
            v.push(root.join("README.md"));
        }
        if root.join("docs").is_dir() {
            v.push(root.join("docs"));
        }
        v
    } else {
        a.paths.iter().map(|p| root.join(p)).collect()
    };
    for r in roots {
        collect_md(&r, &mut files);
    }
    files.sort();
    files
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

fn scan_all(a: &DocsArgs) -> Result<Scanned, String> {
    let root = invoke::workspace_root().map_err(|e| e.to_string())?;
    let mut docs = Vec::new();
    for file in md_files(a, &root) {
        let text =
            std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        // Our own generated fragments are outputs, not sources — don't scan.
        if head_for_sniff(&text, 400).contains("Generated by `cargo soothfast") {
            continue;
        }
        let relpath = rel(&root, &file);
        let doc = markdown::scan(&text).map_err(|e| format!("{relpath}: {e}"))?;
        docs.push((relpath, doc, text));
    }
    Ok(Scanned { root, docs })
}

fn build_surface(a: &DocsArgs) -> Result<(surface::Surface, Value, PathBuf), String> {
    let pkg = a.pkg.as_deref().ok_or("this docs command needs -p PKG")?;
    let (doc, root) =
        invoke::rustdoc_json_in(pkg, None, a.features.as_deref(), invoke::Visibility::Public)
            .map_err(|e| e.to_string())?;
    Ok((surface::from_rustdoc(&doc, &root), doc, root))
}

fn check(a: &DocsArgs) -> i32 {
    let scanned = match scan_all(a) {
        Ok(s) => s,
        Err(e) => return err(&e),
    };
    let mut failures = 0u32;

    // 1. Bind fingerprints vs soothfast.lock.
    let any_binds = scanned.docs.iter().any(|(_, d, _)| !d.binds.is_empty());
    if any_binds {
        let (surf, _, _) = match build_surface(a) {
            Ok(s) => s,
            Err(e) => return err(&format!("binds present but surface unavailable: {e}")),
        };
        let lock = match lockfile::read(&scanned.root) {
            Ok(l) => l,
            Err(e) => return err(&format!("cannot read soothfast.lock: {e}")),
        };
        for (file, doc, _) in &scanned.docs {
            for bind in &doc.binds {
                let Some(info) = surf.items.get(&bind.item) else {
                    failures += 1;
                    println!(
                        "FAIL  {file}:{} bind {}: item not found in crate",
                        bind.line, bind.item
                    );
                    continue;
                };
                let current = format!("{:016x}", info.fingerprint);
                match lock.binds.get(&bind.item) {
                    None => {
                        failures += 1;
                        println!(
                            "FAIL  {file}:{} bind {}: not locked — verify the prose, then `cargo soothfast docs accept -p {}`",
                            bind.line,
                            bind.item,
                            a.pkg.as_deref().unwrap_or("PKG")
                        );
                    }
                    Some(locked) if *locked != current => {
                        failures += 1;
                        println!(
                            "FAIL  {file}:{} bind {}: STALE — code changed under this prose; re-verify, then `docs accept`",
                            bind.line, bind.item
                        );
                    }
                    Some(_) => println!("ok    {file}:{} bind {}", bind.line, bind.item),
                }
            }
        }
    }

    // 2. Claims vs the measurement baseline.
    let any_claims = scanned.docs.iter().any(|(_, d, _)| !d.claims.is_empty());
    if any_claims {
        let baseline = match invoke::load_baseline(&a.baseline) {
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
                    Err(e) => {
                        failures += 1;
                        println!("FAIL  {file}:{} claim `{}`: {e}", cm.line, cm.expr);
                    }
                }
            }
        }
    }

    // 3. Generated-artifact manifest: regenerate in memory, compare to disk.
    if let Some(pkg) = &a.pkg {
        match invoke::pkg_dir(pkg) {
            Err(e) => return err(&e.to_string()),
            Ok(dir) => {
                for (file, doc, _) in &scanned.docs {
                    let stem = gentests::sanitized_stem(file);
                    if let Some(expected) = gentests::test_file(file, doc) {
                        let path = dir.join("tests").join(format!("soothfast_doc_{stem}.rs"));
                        match std::fs::read_to_string(&path) {
                            Ok(actual) if actual == expected => {
                                println!("ok    {file}: generated tests up to date")
                            }
                            _ => {
                                failures += 1;
                                println!(
                                    "FAIL  {file}: generated test missing/stale — run `cargo soothfast docs gen-tests -p {pkg}`"
                                );
                            }
                        }
                    }
                    for (name, expected) in gentests::capture_examples(file, doc) {
                        let path = dir.join("examples").join(format!("{name}.rs"));
                        match std::fs::read_to_string(&path) {
                            Ok(actual) if actual == expected => {}
                            _ => {
                                failures += 1;
                                println!(
                                    "FAIL  {file}: capture example {name} missing/stale — run `docs gen-tests -p {pkg}`"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // 4. Exported binding surface vs soothfast.lock.
    let marked: Vec<(&String, &markdown::ExportMarker)> = scanned
        .docs
        .iter()
        .flat_map(|(file, doc, _)| doc.exports.iter().map(move |m| (file, m)))
        .collect();
    match export_fingerprints(a, !marked.is_empty()) {
        Err(e) => return err(&e),
        Ok(None) => {
            for (file, marker) in &marked {
                failures += 1;
                println!(
                    "FAIL  {file}:{} export {}: the package configures no bindings, so nothing exports it",
                    marker.line, marker.item
                );
            }
        }
        Ok(Some((prefix, fresh))) => {
            for (file, marker) in &marked {
                if fresh.contains_key(&marker.item) {
                    println!("ok    {file}:{} export {}", marker.line, marker.item);
                } else {
                    failures += 1;
                    println!(
                        "FAIL  {file}:{} export {}: not an exported item",
                        marker.line, marker.item
                    );
                }
            }
            let lock = match lockfile::read(&scanned.root) {
                Ok(l) => l,
                Err(e) => return err(&format!("cannot read soothfast.lock: {e}")),
            };
            let pkg = a.pkg.as_deref().unwrap_or("PKG");
            for (id, current) in &fresh {
                match lock.exports.get(id) {
                    None => {
                        failures += 1;
                        println!(
                            "FAIL  export {id}: not locked — review the binding surface, then `cargo soothfast docs accept -p {pkg}`"
                        );
                    }
                    Some(locked) if locked != current => {
                        failures += 1;
                        println!(
                            "FAIL  export {id}: STALE — the bound signature changed; re-review, then `docs accept`"
                        );
                    }
                    Some(_) => println!("ok    export {id}"),
                }
            }
            for id in lock
                .exports
                .keys()
                .filter(|k| k.starts_with(&prefix) && !fresh.contains_key(*k))
            {
                failures += 1;
                println!(
                    "FAIL  export {id}: locked but no longer exported — consumers lose it; `docs accept -p {pkg}` to confirm"
                );
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

/// Contract fingerprints for the package's exported items, and the id prefix
/// they all share.
///
/// `None` when nothing asks for them, which is what keeps a package with no
/// bindings and no export markers from paying for a bench run.
fn export_fingerprints(
    a: &DocsArgs,
    marked: bool,
) -> Result<Option<(String, lockfile::Binds)>, String> {
    let Some(pkg) = a.pkg.as_deref() else {
        return Ok(None);
    };
    let dir = invoke::pkg_dir(pkg).map_err(|e| e.to_string())?;
    if !marked && crate::bind_config::load(&dir)?.entries.is_empty() {
        return Ok(None);
    }
    let mut common = CommonArgs {
        pkg: Some(pkg.to_string()),
        ..CommonArgs::default()
    };
    common.features.clone_from(&a.features);
    let (surface, _) = crate::bind::exported_surface(pkg, &common)?;
    Ok(Some((
        format!("{}::", pkg.replace('-', "_")),
        surface.fingerprints(),
    )))
}

fn accept(a: &DocsArgs) -> i32 {
    let scanned = match scan_all(a) {
        Ok(s) => s,
        Err(e) => return err(&e),
    };
    let (surf, _, _) = match build_surface(a) {
        Ok(s) => s,
        Err(e) => return err(&e),
    };
    let mut fresh = lockfile::Binds::new();
    for (file, doc, _) in &scanned.docs {
        for bind in &doc.binds {
            let Some(info) = surf.items.get(&bind.item) else {
                return err(&format!(
                    "{file}:{} bind {}: item not found in crate",
                    bind.line, bind.item
                ));
            };
            fresh.insert(bind.item.clone(), format!("{:016x}", info.fingerprint));
        }
    }
    let current = match lockfile::read(&scanned.root) {
        Ok(l) => l,
        Err(e) => return err(&format!("cannot read soothfast.lock: {e}")),
    };
    // Explicit PATHS scan a subset of docs: merge so locks for binds in
    // out-of-scope files survive. Full scope replaces (drops dead binds).
    let full_scope = a.paths.is_empty();
    // Exports are not scoped by markdown path, so they re-lock per package
    // whatever the paths were, and a package with none leaves them be.
    let any_marked = scanned.docs.iter().any(|(_, d, _)| !d.exports.is_empty());
    let exports = match export_fingerprints(a, any_marked) {
        Err(e) => return err(&e),
        Ok(Some((prefix, fresh))) => lockfile::merge_scoped(&current.exports, &fresh, &prefix),
        Ok(None) => current.exports.clone(),
    };
    let lock = lockfile::Lock {
        binds: lockfile::merge(&current.binds, &fresh, full_scope),
        exports,
    };
    match lockfile::write(&scanned.root, &lock) {
        Ok(()) => {
            println!(
                "docs accept: locked {} bind(s) and {} export(s) in {}",
                lock.binds.len(),
                lock.exports.len(),
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
    let mut written = 0;
    for (file, doc, _) in &scanned.docs {
        let stem = gentests::sanitized_stem(file);
        if let Some(content) = gentests::test_file(file, doc) {
            let path = dir.join("tests").join(format!("soothfast_doc_{stem}.rs"));
            let _ = std::fs::create_dir_all(path.parent().unwrap());
            if std::fs::write(&path, content).is_err() {
                return err(&format!("cannot write {}", path.display()));
            }
            println!("wrote {}", path.display());
            written += 1;
        }
        for (name, content) in gentests::capture_examples(file, doc) {
            let path = dir.join("examples").join(format!("{name}.rs"));
            let _ = std::fs::create_dir_all(path.parent().unwrap());
            if std::fs::write(&path, content).is_err() {
                return err(&format!("cannot write {}", path.display()));
            }
            println!("wrote {}", path.display());
            written += 1;
        }
    }
    println!("docs gen-tests: {written} file(s)");
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
    let mut failures = 0u32;
    for (file, doc, text) in &scanned.docs {
        let examples = gentests::capture_examples(file, doc);
        if examples.is_empty() {
            continue;
        }
        let mut current = text.clone();
        for (n, (name, _)) in examples.iter().enumerate() {
            let mut cmd = std::process::Command::new("cargo");
            cmd.args(["run", "-q", "-p", &pkg, "--example", name]);
            if let Some(features) = &a.features {
                cmd.args(["--features", features]);
            }
            let out = cmd.stdout(std::process::Stdio::piped()).output();
            let stdout = match out {
                Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
                Ok(o) => {
                    failures += 1;
                    println!("FAIL  {file}: example {name} exited with {}", o.status);
                    continue;
                }
                Err(e) => return err(&format!("cannot run example {name}: {e}")),
            };
            match markdown::splice_output(&current, n, &stdout) {
                Ok(Some(next)) => current = next,
                Ok(None) => {
                    failures += 1;
                    println!("FAIL  {file}: capture block {n} not found for splicing");
                }
                Err(e) => return err(&format!("{file}: {e}")),
            }
        }
        if current != *text {
            if a.check_only {
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
        } else {
            println!("ok    {file}: captured output current");
        }
    }
    if failures > 0 {
        println!("docs capture: FAILED ({failures})");
        1
    } else {
        0
    }
}

fn surface_diff(a: &DocsArgs) -> i32 {
    let Some(pkg) = a.pkg.clone() else {
        return err("diff needs -p PKG");
    };
    let Some(refname) = a.against_ref.clone() else {
        return err("diff needs --against-ref REF");
    };
    let (new_surf, _, _) = match build_surface(a) {
        Ok(s) => s,
        Err(e) => return err(&e),
    };

    let sha = match invoke::git(&["merge-base", "HEAD", &refname]) {
        Ok(s) => s,
        Err(e) => return err(&e.to_string()),
    };
    let root = match invoke::workspace_root() {
        Ok(r) => r,
        Err(e) => return err(&e.to_string()),
    };
    let wt = root.join(".soothfast").join("worktrees").join(&sha);
    if !wt.exists()
        && let Err(e) = invoke::git(&["worktree", "add", "--detach", wt.to_str().unwrap(), &sha])
    {
        return err(&e.to_string());
    }
    // Build the old surface BEFORE removing the worktree: span fingerprints
    // read source files from it.
    let old_surf = match invoke::rustdoc_json_in(
        &pkg,
        Some(&wt),
        a.features.as_deref(),
        invoke::Visibility::Public,
    ) {
        Ok((doc, wt_root)) => surface::from_rustdoc(&doc, &wt_root),
        Err(e) => {
            let _ = invoke::git(&["worktree", "remove", "--force", wt.to_str().unwrap()]);
            return err(&format!("cannot build surface at {sha}: {e}"));
        }
    };
    let _ = invoke::git(&["worktree", "remove", "--force", wt.to_str().unwrap()]);

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
    let (surf, doc, root) = match build_surface(a) {
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
    use super::head_for_sniff;

    #[test]
    fn head_for_sniff_backs_off_to_char_boundary() {
        // '€' is 3 bytes: byte 400 falls mid-char (the old slice panicked).
        let text = "€".repeat(150);
        let head = head_for_sniff(&text, 400);
        assert_eq!(head, "€".repeat(133));
        assert_eq!(head_for_sniff("short", 400), "short");
        assert_eq!(head_for_sniff("abcdef", 3), "abc");
    }
}
