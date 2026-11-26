//! `coverage spec`: for every route and MCP tool a package registers,
//! whether the committed spec declares it and a probe exercises it.
//! Advisory unless `--min PCT` is given.

use serde_json::{Value, json};
use soothfast_spec::specified::{Specified, specified};

use crate::config::{self, Group};
use crate::coverage::pct;
use crate::invoke::{self, CommonArgs};
use crate::spec::gather;
use crate::spec_config;
use crate::spec_probe::{load_probes, probe_covers};

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

fn measure(pkg: &str, common: &CommonArgs, min: Option<u32>, json: bool) -> Result<i32, String> {
    let pkg_dir = invoke::pkg_dir(pkg).map_err(|e| e.to_string())?;
    if spec_config::load(&pkg_dir)?.entries.is_empty() {
        println!("coverage spec: {pkg} has no spec packages");
        return Ok(0);
    }
    let probes = load_probes(&pkg_dir)?;
    let items: Vec<Specified> = gather(pkg, common)?
        .iter()
        .flat_map(|rep| {
            specified(&rep.spec_file, &rep.routes, &rep.rec, |op| {
                probes.iter().any(|p| probe_covers(p, op))
            })
        })
        .collect();
    let total = items.len() as u32;
    let missing = items.iter().filter(|i| !i.is_specified()).count() as u32;
    let p = pct(total, missing);
    if json {
        println!("{}", render_json(&items, p));
    } else {
        print!("{}", render_text(&items, p));
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

fn render_text(items: &[Specified], percent: u32) -> String {
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
            yes_no(i.probed),
        ));
        if let Some(what) = &i.mismatch {
            out.push_str(&format!("            mismatch: {what}\n"));
        }
    }
    let done = items.iter().filter(|i| i.is_specified()).count();
    out.push_str(&format!(
        "coverage: {done}/{} route(s) specified ({percent}%)\n",
        items.len()
    ));
    out
}

fn render_json(items: &[Specified], percent: u32) -> Value {
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(handler: &str, in_spec: bool, probed: bool) -> Specified {
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

    #[test]
    fn text_marks_each_route_and_totals_the_specified_share() {
        let text = render_text(
            &[item("app::a", true, true), item("app::b", true, false)],
            50,
        );
        assert!(text.contains("specified   GET /items/{id} (app::a) in spec: yes, probed: yes"));
        assert!(text.contains("UNSPECIFIED GET /items/{id} (app::b) in spec: yes, probed: no"));
        assert!(text.ends_with("coverage: 1/2 route(s) specified (50%)\n"));
    }

    #[test]
    fn json_carries_the_handler_path_and_both_flags() {
        let v = render_json(&[item("app::a", false, true)], 0);
        assert_eq!(v["percent"], 0);
        assert_eq!(v["routes"][0]["handler"], "app::a");
        assert_eq!(v["routes"][0]["in_spec"], false);
        assert_eq!(v["routes"][0]["probed"], true);
    }
}
