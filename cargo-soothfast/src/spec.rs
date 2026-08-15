//! `cargo soothfast spec check` — reconcile `#[route]` annotations against
//! their declared spec files (OpenAPI / AsyncAPI / GraphQL SDL / MCP tools) —
//! and `cargo soothfast docs routes`, the FastAPI-style endpoint reference
//! rendered from the same reconciliation.

use std::collections::BTreeMap;

use soothfast_spec::{RouteDecl, SpecKind, providers, reconcile, sniff_kind};

use crate::invoke::{self, CommonArgs};

/// One spec file's declared surface reconciled against the route registry.
struct SpecReport {
    spec_file: String,
    kind: SpecKind,
    declared_count: usize,
    routes: Vec<RouteDecl>,
    rec: reconcile::Reconciliation,
}

/// Collect `#[route]` registrations grouped by spec file, parse each spec,
/// and reconcile. Shared by `spec check` and `docs routes`.
fn gather(pkg: &str, common: &CommonArgs) -> Result<Vec<SpecReport>, String> {
    let records = invoke::run_bench(common, &["--list-routes"]).map_err(|e| e.to_string())?;
    let mut by_spec: BTreeMap<String, Vec<RouteDecl>> = BTreeMap::new();
    for r in records.iter().filter(|r| r["type"] == "route") {
        let route = RouteDecl {
            id: r["id"].as_str().unwrap_or("").to_string(),
            operation: r["operation"].as_str().unwrap_or("").to_string(),
            method: r["method"].as_str().unwrap_or("").to_string(),
            path: r["path"].as_str().unwrap_or("").to_string(),
        };
        by_spec
            .entry(r["spec"].as_str().unwrap_or("").to_string())
            .or_default()
            .push(route);
    }
    if by_spec.is_empty() {
        return Err(format!("no #[route] annotations registered in {pkg}"));
    }
    let pkg_dir = invoke::pkg_dir(pkg).map_err(|e| e.to_string())?;

    let mut reports = Vec::new();
    for (spec_file, routes) in by_spec {
        let path = pkg_dir.join(&spec_file);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read spec {}: {e}", path.display()))?;
        let kind = sniff_kind(&spec_file, &text)
            .ok_or_else(|| format!("cannot determine spec dialect of {spec_file}"))?;
        let declared = providers::parse(kind, &text).map_err(|e| format!("{spec_file}: {e}"))?;
        let rec = reconcile::reconcile(&declared, &routes);
        reports.push(SpecReport {
            spec_file,
            kind,
            declared_count: declared.len(),
            routes,
            rec,
        });
    }
    Ok(reports)
}

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("gen") => crate::spec_gen::run(&args[1..]),
        Some("gate") => crate::spec_gate::run(&args[1..]),
        Some("check") => run_check(&args[1..]),
        Some("check-proto") => run_check_proto(&args[1..]),
        Some("html") => crate::spec_html::run(&args[1..]),
        Some("probe") => crate::spec_probe::run(&args[1..]),
        _ => {
            eprintln!(
                "soothfast: usage: cargo soothfast spec gen -p PKG [--check]\n\
                 cargo soothfast spec gate -p PKG [--base REF] [--from-committed]\n\
                 cargo soothfast spec check -p PKG\n\
                 cargo soothfast spec check-proto -p PKG --proto FILE --message NAME --source FILE --struct NAME\n\
                 cargo soothfast spec html -p PKG [--features F] [--target NAME] [--out DIR]\n\
                 cargo soothfast spec probe -p PKG [--accept] [--allow-gone] [--base-url URL] [--filter SUBSTR]"
            );
            2
        }
    }
}

fn run_check(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if !common.try_parse(a, &mut it) {
            eprintln!("soothfast: unknown spec arg {a:?}");
            return 2;
        }
    }
    let Some(pkg) = common.pkg.clone() else {
        eprintln!("soothfast: spec check requires -p PKG");
        return 2;
    };
    let reports = match gather(&pkg, &common) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 1;
        }
    };

    let mut failures = 0u32;
    for rep in &reports {
        let spec_file = &rep.spec_file;
        println!(
            "spec: {spec_file} ({:?}) — {} declared, {} matched",
            rep.kind, rep.declared_count, rep.rec.matched
        );
        for d in &rep.rec.missing_handler {
            failures += 1;
            println!(
                "FAIL  {spec_file}: `{}` ({} {}) has no #[route] handler",
                d.operation, d.method, d.path
            );
        }
        for r in &rep.rec.missing_spec {
            failures += 1;
            println!(
                "FAIL  {spec_file}: {} declares `{}` — not in the spec",
                r.id, r.operation
            );
        }
        for (r, _d, what) in &rep.rec.mismatched {
            failures += 1;
            println!(
                "FAIL  {spec_file}: `{}` ({}) mismatch: {what}",
                r.operation, r.id
            );
        }
    }

    if failures > 0 {
        println!("spec check: FAILED ({failures} problem(s))");
        1
    } else {
        println!("spec check: passed ({} spec file(s))", reports.len());
        0
    }
}

/// `cargo soothfast spec check-proto -p PKG --proto FILE --message NAME
/// --source FILE --struct NAME` — see `soothfast_spec::proto`.
fn run_check_proto(args: &[String]) -> i32 {
    let mut pkg: Option<String> = None;
    let mut proto_path: Option<String> = None;
    let mut message: Option<String> = None;
    let mut source_path: Option<String> = None;
    let mut struct_name: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let slot = match a.as_str() {
            "-p" | "--package" => &mut pkg,
            "--proto" => &mut proto_path,
            "--message" => &mut message,
            "--source" => &mut source_path,
            "--struct" => &mut struct_name,
            other => {
                eprintln!("soothfast: unknown spec check-proto arg {other:?}");
                return 2;
            }
        };
        *slot = it.next().cloned();
    }
    let (Some(pkg), Some(proto_path), Some(message), Some(source_path), Some(struct_name)) =
        (pkg, proto_path, message, source_path, struct_name)
    else {
        eprintln!(
            "soothfast: usage: cargo soothfast spec check-proto -p PKG --proto FILE \
             --message NAME --source FILE --struct NAME"
        );
        return 2;
    };

    let pkg_dir = match invoke::pkg_dir(&pkg) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 1;
        }
    };
    let proto_text = match std::fs::read_to_string(pkg_dir.join(&proto_path)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("soothfast: cannot read {proto_path}: {e}");
            return 1;
        }
    };
    let source_text = match std::fs::read_to_string(pkg_dir.join(&source_path)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("soothfast: cannot read {source_path}: {e}");
            return 1;
        }
    };
    let spec_fields = match soothfast_spec::proto::parse_proto_message(&proto_text, &message) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("soothfast: {proto_path}: {e}");
            return 1;
        }
    };
    let code_fields =
        match soothfast_spec::proto::parse_struct_prost_fields(&source_text, &struct_name) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("soothfast: {source_path}: {e}");
                return 1;
            }
        };

    let rec = soothfast_spec::proto::reconcile_proto(&spec_fields, &code_fields);
    println!(
        "spec-proto: {proto_path} `{message}` vs {source_path} `{struct_name}` — {} declared, {} matched",
        spec_fields.len(),
        rec.matched
    );
    let mut failures = 0u32;
    for f in &rec.missing_code {
        failures += 1;
        println!(
            "FAIL  {proto_path}: tag {} (`{}` {}) has no matching #[prost] field in {struct_name}",
            f.tag, f.name, f.ty
        );
    }
    for f in &rec.missing_spec {
        failures += 1;
        println!(
            "FAIL  {source_path}: {struct_name}.{} declares tag {} — not in {message}",
            f.name, f.tag
        );
    }
    for (spec, code, what) in &rec.mismatched {
        failures += 1;
        println!(
            "FAIL  tag {} (`{}` in {message} / `{}` in {struct_name}): {what}",
            spec.tag, spec.name, code.name
        );
    }

    if failures > 0 {
        println!("spec check-proto: FAILED ({failures} problem(s))");
        1
    } else {
        println!("spec check-proto: passed");
        0
    }
}

/// `cargo soothfast docs routes -p PKG [--baseline NAME] [--out DIR]` — the
/// human-facing endpoint reference, FastAPI-style: every operation with its
/// handler's doc comment, reconciliation status, and measured cost. Produced
/// from the same registry + spec parse the gate uses, so it cannot drift.
pub fn routes_cmd(
    pkg: &str,
    baseline_name: &str,
    out: Option<&std::path::Path>,
    features: Option<&str>,
    target: Option<&str>,
) -> i32 {
    let common = CommonArgs {
        pkg: Some(pkg.to_string()),
        features: features.map(str::to_string),
        target: target.map(str::to_string),
        ..Default::default()
    };
    let reports = match gather(pkg, &common) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 1;
        }
    };
    // Handler doc comments from rustdoc; measured facts from the baseline.
    // Optional enrichment: a bin-only crate (no `[lib]` target, e.g. one
    // whose handlers can't carry `#[route]` directly for the same reason)
    // has no rustdoc surface to pull comments from — the reconciliation
    // table itself doesn't need them, so degrade instead of failing.
    let docs_text = match crate::docs_support::load(pkg, features) {
        Ok((_, doc)) => crate::docs_support::docs_text(&doc),
        Err(e) => {
            eprintln!("soothfast: handler doc comments unavailable, continuing without them: {e}");
            std::collections::BTreeMap::new()
        }
    };
    let baseline = invoke::load_baseline(baseline_name)
        .ok()
        .flatten()
        .unwrap_or(serde_json::Value::Null);

    let mut out_md = format!(
        "# `{pkg}` endpoints\n\n> Generated by `cargo soothfast docs routes`\n> Every operation is reconciled against its spec file in CI; drift fails the build.\n"
    );
    for rep in &reports {
        out_md.push_str(&format!(
            "\n## `{}` <sup>{:?}</sup>\n",
            rep.spec_file, rep.kind
        ));
        out_md.push_str(&format!(
            "\n{}\n",
            gauge_html(rep.rec.matched, rep.declared_count)
        ));

        let mut routes = rep.routes.clone();
        routes.sort_by(|a, b| (&a.path, &a.method).cmp(&(&b.path, &b.method)));
        let (mut problem_rows, mut problem_count) = (String::new(), 0usize);
        let (mut ok_rows, mut ok_count) = (String::new(), 0usize);
        for r in &routes {
            let status = route_status(&rep.rec, r);
            let docs = docs_text.get(&r.id).map(String::as_str);
            let measured = measured_facts(&baseline, &r.id);
            let row = route_row(
                &status,
                &r.method,
                &r.path,
                &r.operation,
                &r.id,
                docs,
                measured.as_deref(),
            );
            if status.pass {
                ok_rows.push_str(&row);
                ok_count += 1;
            } else {
                problem_rows.push_str(&row);
                problem_count += 1;
            }
        }
        for d in &rep.rec.missing_handler {
            let status = RouteStatus {
                pass: false,
                mark: "✗",
                label: "missing handler".to_string(),
            };
            problem_rows.push_str(&route_row(
                &status,
                &d.method,
                &d.path,
                &d.operation,
                "—",
                None,
                None,
            ));
            problem_count += 1;
        }

        if problem_count > 0 {
            let word = if problem_count == 1 {
                "problem"
            } else {
                "problems"
            };
            out_md.push_str(&route_group(word, problem_count, true, &problem_rows));
        }
        if ok_count > 0 {
            out_md.push_str(&route_group(
                "reconciled",
                ok_count,
                problem_count == 0,
                &ok_rows,
            ));
        }
    }

    let root = match invoke::workspace_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 1;
        }
    };
    let out_dir = out
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| root.join("docs/routes"));
    let _ = std::fs::create_dir_all(&out_dir);
    let path = out_dir.join(format!("{pkg}.md"));
    match std::fs::write(&path, out_md) {
        Ok(()) => {
            println!("docs routes: wrote {}", path.display());
            0
        }
        Err(e) => {
            eprintln!("soothfast: cannot write {}: {e}", path.display());
            1
        }
    }
}

/// One route's reconciliation verdict, ready to render — parsed out of
/// `route_status` once so both grouping (pass/fail) and display (mark +
/// label) read from the same source.
struct RouteStatus {
    pass: bool,
    mark: &'static str,
    label: String,
}

fn route_status(rec: &reconcile::Reconciliation, r: &RouteDecl) -> RouteStatus {
    if let Some((_, _, what)) = rec
        .mismatched
        .iter()
        .find(|(m, _, _)| m.id == r.id && m.operation == r.operation)
    {
        return RouteStatus {
            pass: false,
            mark: "✗",
            label: format!("mismatch: {what}"),
        };
    }
    if rec
        .missing_spec
        .iter()
        .any(|m| m.id == r.id && m.operation == r.operation)
    {
        return RouteStatus {
            pass: false,
            mark: "✗",
            label: "not in the spec".into(),
        };
    }
    RouteStatus {
        pass: true,
        mark: "✓",
        label: "reconciled".into(),
    }
}

/// Joined `label value` facts for a handler, from either a direct
/// measurement or a `covers`-linked bench (direct wins on overlap). `None`
/// when nothing in the baseline measures this handler at all.
fn measured_facts(baseline: &serde_json::Value, handler_id: &str) -> Option<String> {
    let items = baseline["items"].as_object()?;
    let mut seen: Vec<&str> = Vec::new();
    let mut facts = Vec::new();
    let matches = |id: &str, m: &serde_json::Value, exact: bool| {
        let target = m["covers"].as_str().filter(|c| !c.is_empty()).unwrap_or(id);
        if exact {
            id == handler_id
        } else {
            target == handler_id && id != handler_id
        }
    };
    for exact in [true, false] {
        for (id, m) in items {
            if !matches(id, m, exact) {
                continue;
            }
            for (label, keys) in [
                ("instructions/iter", ["perfcnt", "instructions"]),
                ("median ns", ["walltime", "median_ns"]),
                ("allocs/iter", ["alloc", "allocs"]),
                ("polls/iter", ["asyncexec", "polls"]),
                ("wakes/iter", ["asyncexec", "wakes"]),
            ] {
                if seen.contains(&label) {
                    continue;
                }
                if let Some(v) = m[keys[0]][keys[1]].as_f64() {
                    seen.push(label);
                    facts.push(format!("{v:.0} {label}"));
                }
            }
        }
    }
    (!facts.is_empty()).then(|| facts.join(" · "))
}

/// A calibrated rail-and-fill gauge (light-page palette, unlike the dark
/// code-panel `chip-scale` in soothfast-site) showing reconciled coverage
/// as a real measured proportion, not just stated in prose.
fn gauge_html(matched: usize, declared: usize) -> String {
    let frac = if declared == 0 {
        0.0
    } else {
        matched as f64 / declared as f64
    };
    let (x0, x1) = (4.0_f64, 216.0_f64);
    let fill_x = x0 + frac * (x1 - x0);
    let fill_class = if matched == declared {
        "route-gauge-fill"
    } else {
        "route-gauge-fill has-gaps"
    };
    format!(
        "<div class=\"route-summary\">\
<svg class=\"route-gauge-svg\" viewBox=\"0 0 220 14\" aria-hidden=\"true\" focusable=\"false\">\
<line x1=\"{x0}\" y1=\"7\" x2=\"{x1}\" y2=\"7\" class=\"route-gauge-rail\"/>\
<line x1=\"{x0}\" y1=\"7\" x2=\"{fill_x:.1}\" y2=\"7\" class=\"{fill_class}\"/>\
</svg>\
<span class=\"route-summary-text\"><strong>{matched}</strong>/{declared} reconciled</span>\
</div>"
    )
}

/// One `<tr>` for the route table: status, method, path, operation, handler
/// — dense enough that a hundred routes still scan at a glance. `docs` (a
/// hover tooltip, when the handler's own doc comment is reachable) and
/// `measured` (an inline fact, when the handler is a measured item too)
/// are both optional enrichment, not load-bearing columns.
fn route_row(
    status: &RouteStatus,
    method: &str,
    path: &str,
    operation: &str,
    handler_id: &str,
    docs: Option<&str>,
    measured: Option<&str>,
) -> String {
    let state_class = if status.pass {
        "route-status-pass"
    } else {
        "route-status-fail"
    };
    let title = docs
        .filter(|d| !d.trim().is_empty())
        .map(|d| format!(" title=\"{}\"", escape(d.trim())))
        .unwrap_or_default();
    let measured_html = measured
        .map(|m| format!(" <span class=\"route-measured\">· {}</span>", escape(m)))
        .unwrap_or_default();
    format!(
        "<tr><td><span class=\"route-status {state_class}\">{} {}</span></td>\
<td><code>{}</code></td><td><code>{}</code></td>\
<td{title}>{}{measured_html}</td><td><code>{}</code></td></tr>",
        status.mark,
        escape(&status.label),
        escape(method),
        escape(if path.is_empty() { "—" } else { path }),
        escape(operation),
        escape(handler_id),
    )
}

/// A collapsible group of route rows — problems open by default (that's
/// what a reader landed on the page to see), a fully-clean group collapsed
/// (nothing to act on) unless it's the only group on the page. `label` is
/// the already-correctly-inflected noun/adjective phrase for `count`
/// ("1 problem" / "3 problems" / "N reconciled" — "reconciled" doesn't
/// pluralize at all, so this doesn't try to guess an `-s` suffix for it).
fn route_group(label: &str, count: usize, open: bool, rows_html: &str) -> String {
    format!(
        "\n<details class=\"route-group\"{}>\
<summary><svg class=\"route-group-chevron\" width=\"11\" height=\"11\" viewBox=\"0 0 16 16\" fill=\"none\" aria-hidden=\"true\">\
<path d=\"M5 2.5 L11 8 L5 13.5\" stroke=\"currentColor\" stroke-width=\"1.6\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/></svg>\
{count} {label}</summary>\
<div class=\"tablewrap\"><table><thead><tr><th>status</th><th>method</th><th>path</th><th>operation</th><th>handler</th></tr></thead>\
<tbody>{rows_html}</tbody></table></div></details>\n",
        if open { " open" } else { "" },
    )
}

pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}
