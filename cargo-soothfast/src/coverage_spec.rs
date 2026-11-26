//! `coverage spec`: for every route and MCP tool a package registers,
//! whether the committed spec declares it and a probe exercises it.
//! Advisory unless `--min PCT` is given.

use serde_json::{Value, json};
use soothfast_spec::DeclaredOp;
use soothfast_spec::specified::{Specified, specified};

use crate::config::{self, Group};
use crate::coverage::pct;
use crate::invoke::{self, CommonArgs};
use crate::spec::{SpecReport, gather_registered};
use crate::spec_config::{self, SpecEntry};
use crate::spec_probe::{load_probes, probe_covers};

/// A spec operation no registered route implements, with its spec file.
type Unhandled = (String, DeclaredOp);

pub fn run(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut min: Option<u32> = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--min" => match it.next().and_then(|v| v.parse().ok()) {
                Some(v) => min = Some(v),
                None => {
                    eprintln!("soothfast: --min needs a whole percent");
                    return 2;
                }
            },
            _ => {
                if !common.try_parse(a, &mut it) {
                    eprintln!("soothfast: unknown coverage spec arg {a:?}");
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
        eprintln!("soothfast: coverage spec requires -p PKG");
        return 2;
    };
    match measure(&pkg, &common, min, json) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("soothfast: {e}");
            1
        }
    }
}

/// What a package registers against its spec files.
struct Collected {
    entries: Vec<SpecEntry>,
    reports: Vec<SpecReport>,
    items: Vec<Specified>,
    unhandled: Vec<Unhandled>,
}

fn collect(pkg: &str, common: &CommonArgs) -> Result<Collected, String> {
    let pkg_dir = invoke::pkg_dir(pkg).map_err(|e| e.to_string())?;
    let entries = spec_config::load(&pkg_dir)?.entries;
    let mut collected = Collected {
        entries,
        reports: Vec::new(),
        items: Vec::new(),
        unhandled: Vec::new(),
    };
    if collected.entries.is_empty() {
        return Ok(collected);
    }
    let probes = load_probes(&pkg_dir)?;
    collected.reports = gather_registered(pkg, common)?;
    for rep in &collected.reports {
        collected
            .items
            .extend(specified(&rep.spec_file, &rep.routes, &rep.rec, |op| {
                probes.iter().any(|p| probe_covers(p, op))
            }));
        collected.unhandled.extend(
            rep.rec
                .missing_handler
                .iter()
                .map(|op| (rep.spec_file.clone(), op.clone())),
        );
    }
    Ok(collected)
}

/// The routes and tools `pkg` registers, each with whether it is specified.
/// Empty for a package with no spec packages.
pub(crate) fn specified_items(pkg: &str, common: &CommonArgs) -> Result<Vec<Specified>, String> {
    collect(pkg, common).map(|c| c.items)
}

fn measure(pkg: &str, common: &CommonArgs, min: Option<u32>, json: bool) -> Result<i32, String> {
    let Collected {
        entries,
        reports,
        items,
        unhandled,
    } = collect(pkg, common)?;
    if entries.is_empty() {
        println!("coverage spec: {pkg} has no spec packages");
        return Ok(0);
    }
    let total = items.len() as u32;
    let missing = items.iter().filter(|i| !i.is_specified()).count() as u32;
    let p = pct(total, missing);
    if json {
        println!("{}", render_json(&items, &unhandled, p));
    } else {
        for entry in &entries {
            if !reports.iter().any(|r| r.spec_file == entry.path) {
                println!("{pkg}: no routes registered for {}", entry.path);
            }
        }
        print!("{}", render_text(&items, &unhandled, p));
    }
    if let Some(min) = min
        && p < min
    {
        println!("coverage spec: FAILED ({p}% < required {min}%)");
        return Ok(1);
    }
    Ok(0)
}

fn yes_no(flag: bool) -> &'static str {
    if flag { "yes" } else { "no" }
}

fn probed_label(probed: Option<bool>) -> &'static str {
    probed.map_or("n/a", yes_no)
}

fn render_text(items: &[Specified], unhandled: &[Unhandled], percent: u32) -> String {
    let mut out = String::new();
    for i in items {
        let mark = if i.is_specified() {
            "specified  "
        } else {
            "UNSPECIFIED"
        };
        out.push_str(&format!(
            "{mark} {} {} ({}) in spec: {}, probed: {}\n",
            i.method,
            i.path,
            i.handler,
            yes_no(i.in_spec),
            probed_label(i.probed),
        ));
        if let Some(what) = &i.mismatch {
            out.push_str(&format!("            mismatch: {what}\n"));
        }
    }
    for (_, op) in unhandled {
        out.push_str(&format!(
            "declared, no handler: {} {} {}\n",
            op.operation, op.method, op.path
        ));
    }
    let done = items.iter().filter(|i| i.is_specified()).count();
    out.push_str(&format!(
        "coverage: {done}/{} route(s) specified ({percent}%)\n",
        items.len()
    ));
    out
}

fn render_json(items: &[Specified], unhandled: &[Unhandled], percent: u32) -> Value {
    json!({
        "percent": percent,
        "routes": items.iter().map(|i| json!({
            "spec": i.spec,
            "operation": i.operation,
            "method": i.method,
            "path": i.path,
            "handler": i.handler,
            "in_spec": i.in_spec,
            "mismatch": i.mismatch,
            "probed": i.probed,
        })).collect::<Vec<_>>(),
        "unhandled": unhandled.iter().map(|(spec, op)| json!({
            "spec": spec,
            "operation": op.operation,
            "method": op.method,
            "path": op.path,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(handler: &str, in_spec: bool, probed: Option<bool>) -> Specified {
        Specified {
            spec: "api.yaml".into(),
            operation: "getItem".into(),
            method: "GET".into(),
            path: "/items/{id}".into(),
            handler: handler.into(),
            in_spec,
            mismatch: None,
            probed,
        }
    }

    fn orphan() -> Unhandled {
        (
            "api.yaml".into(),
            DeclaredOp {
                operation: "listItems".into(),
                method: "GET".into(),
                path: "/items".into(),
            },
        )
    }

    #[test]
    fn text_marks_each_route_and_totals_the_specified_share() {
        let text = render_text(
            &[
                item("app::a", true, Some(true)),
                item("app::b", true, Some(false)),
                item("app::c", true, None),
            ],
            &[],
            67,
        );
        assert!(text.contains("specified   GET /items/{id} (app::a) in spec: yes, probed: yes"));
        assert!(text.contains("UNSPECIFIED GET /items/{id} (app::b) in spec: yes, probed: no"));
        assert!(text.contains("specified   GET /items/{id} (app::c) in spec: yes, probed: n/a"));
        assert!(text.ends_with("coverage: 2/3 route(s) specified (67%)\n"));
    }

    #[test]
    fn text_lists_unhandled_operations_outside_the_total() {
        let text = render_text(&[], &[orphan()], 100);
        assert!(text.contains("declared, no handler: listItems GET /items\n"));
        assert!(text.ends_with("coverage: 0/0 route(s) specified (100%)\n"));
    }

    #[test]
    fn json_carries_the_handler_path_flags_and_unhandled_operations() {
        let v = render_json(&[item("app::a", false, None)], &[orphan()], 0);
        assert_eq!(v["percent"], 0);
        assert_eq!(v["routes"][0]["handler"], "app::a");
        assert_eq!(v["routes"][0]["in_spec"], false);
        assert_eq!(v["routes"][0]["probed"], Value::Null);
        assert_eq!(v["unhandled"][0]["operation"], "listItems");
    }
}
