//! `coverage report`: one row per public item with the dimensions it is
//! covered on. A dimension a tree cannot tell is `n/a`, never 0%.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Value, json};
use soothfast_docs::{claims, markdown, surface::Surface};

use crate::config::{self, Group};
use crate::coverage::{self, dangling_covers, declared_covers};
use crate::coverage_lang;
use crate::invoke::{self, CommonArgs};
use crate::{docs, workspace};

/// A public item and what covers it. `None` is a dimension the item's tree
/// has no way to tell.
struct Row {
    path: String,
    kind: String,
    documented: bool,
    documented_strict: Option<bool>,
    measured_declared: Option<bool>,
}

const DIMENSIONS: [(&str, &str); 3] = [
    ("documented", "documented"),
    ("documented_strict", "documented+"),
    ("measured_declared", "measured (declared)"),
];

impl Row {
    fn dimension(&self, name: &str) -> Option<bool> {
        match name {
            "documented" => Some(self.documented),
            "documented_strict" => self.documented_strict,
            _ => self.measured_declared,
        }
    }
}

pub fn run(args: &[String]) -> i32 {
    let mut pkgs: Vec<String> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    let mut lang: Option<String> = None;
    let mut features: Option<String> = None;
    let mut json = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-p" | "--package" => pkgs.extend(it.next().cloned()),
            "--dir" => dirs.extend(it.next().cloned()),
            "--lang" => lang = it.next().cloned(),
            "--features" => features = it.next().cloned(),
            "--json" => json = true,
            other => {
                eprintln!("soothfast: unknown coverage report arg {other:?}");
                return 2;
            }
        }
    }
    match rows(&pkgs, &dirs, lang.as_deref(), features) {
        Ok(rows) => {
            if json {
                println!("{:#}", render_json(&rows));
            } else {
                print!("{}", render_text(&rows));
            }
            0
        }
        Err((code, e)) => {
            eprintln!("soothfast: {e}");
            code
        }
    }
}

type Failure = (i32, String);

fn rows(
    pkgs: &[String],
    dirs: &[String],
    lang: Option<&str>,
    features: Option<String>,
) -> Result<Vec<Row>, Failure> {
    let defaults = config::coverage_defaults(pkgs).map_err(|e| (2, e))?;
    let features = features.or(defaults.features);
    let targets = coverage::lang_targets(pkgs, dirs, lang).map_err(|e| (2, e))?;
    let mut rows = Vec::new();
    for target in &targets {
        let entries = coverage_lang::entries(target).map_err(|e| (1, e))?;
        rows.extend(entries.into_iter().map(|e| Row {
            path: e.id,
            kind: e.kind,
            documented: e.documented,
            documented_strict: None,
            measured_declared: None,
        }));
    }
    if pkgs.is_empty() {
        return Ok(rows);
    }
    let marked = marker_targets(&invoke::project_root());
    for pkg in pkgs {
        let surf = coverage::load_surface(pkg, features.as_deref()).map_err(|e| (1, e))?;
        let records = discover(pkg, features.as_deref()).map_err(|e| (1, e))?;
        for warning in dangling_covers(&records, &surf, pkg) {
            eprintln!("{warning}");
        }
        rows.extend(rust_rows(
            &surf,
            &declared_covers(&records),
            &marked.items(&records),
        ));
    }
    Ok(rows)
}

/// The registry's items for `pkg`; none when it has no bench target to link
/// them from.
fn discover(pkg: &str, features: Option<&str>) -> Result<Vec<Value>, String> {
    let mut common = CommonArgs {
        pkg: Some(pkg.to_string()),
        features: features.map(str::to_string),
        ..CommonArgs::default()
    };
    config::apply(&mut common, Group::Docs)?;
    let target = common.target.as_deref().unwrap_or("soothfast");
    if !workspace::has_bench_target(pkg, target, None).map_err(|e| e.to_string())? {
        return Ok(Vec::new());
    }
    invoke::run_discovery(&common, &["--list"]).map_err(|e| e.to_string())
}

/// Every item and method of `surf`. `documented_strict` implies `documented`:
/// an example or a marker lifts a documented item, it does not stand in for
/// the docs.
fn rust_rows(surf: &Surface, declared: &BTreeSet<String>, marked: &BTreeSet<String>) -> Vec<Row> {
    let named = |set: &BTreeSet<String>, aliases: &[&String]| {
        aliases.iter().any(|a| set.contains(a.as_str()))
    };
    surf.counted()
        .into_iter()
        .map(|c| Row {
            path: c.path.clone(),
            kind: c.info.kind.clone(),
            documented: c.info.has_docs,
            documented_strict: Some(
                c.info.has_docs && (c.info.example || named(marked, &c.aliases)),
            ),
            measured_declared: (c.info.kind == "function").then(|| named(declared, &c.aliases)),
        })
        .collect()
}

/// What the repo's tracked markdown names: Rust items its binds target, and
/// the bench ids its claims measure.
#[derive(Default)]
struct Markers {
    binds: BTreeSet<String>,
    claimed: BTreeSet<String>,
}

impl Markers {
    /// The items markers cover: bound ones, and what a claimed bench `covers`
    /// (its own id where that is empty).
    fn items(&self, records: &[Value]) -> BTreeSet<String> {
        let claimed = coverage::benches(records)
            .filter(|(id, _)| self.claimed.contains(*id))
            .map(|(id, covers)| coverage::measured_path(id, covers));
        self.binds
            .iter()
            .map(String::as_str)
            .chain(claimed)
            .map(str::to_string)
            .collect()
    }
}

fn marker_targets(root: &Path) -> Markers {
    let texts = docs::tracked_md(root)
        .into_iter()
        .filter_map(|f| std::fs::read_to_string(root.join(f)).ok());
    targets_in(texts)
}

fn targets_in(texts: impl Iterator<Item = String>) -> Markers {
    let mut out = Markers::default();
    for doc in texts.filter_map(|t| markdown::scan(&t).ok()) {
        out.binds.extend(
            doc.binds
                .iter()
                .flat_map(|b| b.targets())
                .map(str::to_string),
        );
        for claim in doc
            .claims
            .iter()
            .filter_map(|c| claims::parse(&c.expr).ok())
        {
            out.claimed.extend(claim.minus.map(|m| m.item));
            out.claimed.insert(claim.item);
        }
    }
    out
}

/// Covered and total over the rows a dimension applies to; `None` when it
/// applies to none.
fn summarize(rows: &[Row], name: &str) -> Option<(u32, u32)> {
    let applicable: Vec<bool> = rows.iter().filter_map(|r| r.dimension(name)).collect();
    if applicable.is_empty() {
        return None;
    }
    let covered = applicable.iter().filter(|c| **c).count() as u32;
    Some((covered, applicable.len() as u32))
}

fn percent((covered, total): (u32, u32)) -> u32 {
    coverage::pct(total, total - covered)
}

fn render_json(rows: &[Row]) -> Value {
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "path": r.path,
                "kind": r.kind,
                "documented": r.documented,
                "documented_strict": r.documented_strict,
                "measured_declared": r.measured_declared,
            })
        })
        .collect();
    let summary: serde_json::Map<String, Value> = DIMENSIONS
        .iter()
        .map(|(name, _)| {
            let value = summarize(rows, name).map_or(
                Value::Null,
                |s| json!({ "covered": s.0, "total": s.1, "pct": percent(s) }),
            );
            (name.to_string(), value)
        })
        .collect();
    json!({ "items": items, "summary": summary })
}

fn render_text(rows: &[Row]) -> String {
    let cell = |v: Option<bool>| match v {
        Some(true) => "yes",
        Some(false) => "no",
        None => "n/a",
    };
    let mut out = format!(
        "{:<10} {:<11} {:<19} item\n",
        "documented", "documented+", "measured (declared)"
    );
    for r in rows {
        out.push_str(&format!(
            "{:<10} {:<11} {:<19} {} ({})\n",
            cell(Some(r.documented)),
            cell(r.documented_strict),
            cell(r.measured_declared),
            r.path,
            r.kind
        ));
    }
    for (name, label) in DIMENSIONS {
        let figure = summarize(rows, name).map_or("n/a".to_string(), |s| {
            format!("{}/{} ({}%)", s.0, s.1, percent(s))
        });
        out.push_str(&format!("{:<20} {figure}\n", format!("{label}:")));
    }
    out
}

#[cfg(test)]
mod tests {
    use soothfast_docs::surface::ItemInfo;

    use super::*;

    fn info(kind: &str, fingerprint: u64, has_docs: bool, example: bool) -> ItemInfo {
        ItemInfo {
            kind: kind.into(),
            fingerprint,
            signature: String::new(),
            has_docs,
            example,
            spans: Vec::new(),
        }
    }

    fn surface() -> Surface {
        let mut s = Surface::default();
        s.items
            .insert("d::sorted".into(), info("function", 1, true, true));
        s.items
            .insert("d::alias::sorted".into(), info("function", 1, true, true));
        s.items
            .insert("d::bound".into(), info("function", 2, true, false));
        s.items
            .insert("d::alias::bound".into(), info("function", 2, true, false));
        s.items
            .insert("d::bare".into(), info("function", 3, true, false));
        s.items
            .insert("d::blank".into(), info("function", 4, false, false));
        s.items
            .insert("d::Thing".into(), info("struct", 5, true, false));
        s.members
            .insert("d::Thing::go".into(), info("function", 6, true, false));
        s
    }

    fn set(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    fn row<'a>(rows: &'a [Row], path: &str) -> &'a Row {
        rows.iter().find(|r| r.path == path).unwrap()
    }

    #[test]
    fn a_fenced_example_or_a_bind_through_an_alias_lifts_a_documented_item() {
        let rows = rust_rows(&surface(), &set(&[]), &set(&["d::alias::bound"]));
        assert_eq!(row(&rows, "d::sorted").documented_strict, Some(true));
        assert_eq!(row(&rows, "d::bound").documented_strict, Some(true));
        assert_eq!(row(&rows, "d::bare").documented_strict, Some(false));
    }

    #[test]
    fn a_marker_does_not_stand_in_for_the_docs() {
        let rows = rust_rows(&surface(), &set(&[]), &set(&["d::blank"]));
        let blank = row(&rows, "d::blank");
        assert_eq!(
            (blank.documented, blank.documented_strict),
            (false, Some(false))
        );
    }

    #[test]
    fn only_callables_are_declared_measured() {
        let rows = rust_rows(
            &surface(),
            &set(&["d::alias::sorted", "d::Thing::go"]),
            &set(&[]),
        );
        assert_eq!(row(&rows, "d::sorted").measured_declared, Some(true));
        assert_eq!(row(&rows, "d::Thing::go").measured_declared, Some(true));
        assert_eq!(row(&rows, "d::bare").measured_declared, Some(false));
        assert_eq!(row(&rows, "d::Thing").measured_declared, None);
    }

    #[test]
    fn markers_name_binds_and_claimed_bench_ids() {
        let md = "<!-- soothfast:bind d::a d::b -->\n<!-- soothfast:claim d::c.walltime.p99 < 1ms -->\n<!-- soothfast:claim d::x.alloc.allocs - d::y.alloc.allocs < 2 -->\n";
        let got = targets_in([md.to_string(), "```\nunclosed".to_string()].into_iter());
        assert_eq!(got.binds, set(&["d::a", "d::b"]));
        assert_eq!(got.claimed, set(&["d::c", "d::x", "d::y"]));
    }

    #[test]
    fn a_claim_lifts_the_item_its_bench_covers() {
        let md = "<!-- soothfast:claim demo::bench_run.alloc.allocs <= 8 -->\n<!-- soothfast:claim demo::bench_self.alloc.allocs <= 8 -->\n";
        let markers = targets_in([md.to_string()].into_iter());
        let records = [
            json!({ "type": "item", "id": "demo::bench_run", "covers": "demo::run" }),
            json!({ "type": "item", "id": "demo::bench_self", "covers": "" }),
            json!({ "type": "item", "id": "demo::bench_idle", "covers": "demo::idle" }),
        ];
        assert_eq!(
            markers.items(&records),
            set(&["demo::run", "demo::bench_self"])
        );
        let mut surf = Surface::default();
        surf.items
            .insert("demo::run".into(), info("function", 1, true, false));
        let rows = rust_rows(&surf, &set(&[]), &markers.items(&records));
        assert_eq!(row(&rows, "demo::run").documented_strict, Some(true));
    }

    #[test]
    fn a_dimension_no_row_can_tell_is_not_applicable() {
        let rows = vec![Row {
            path: "src/a.py#f".into(),
            kind: "function".into(),
            documented: true,
            documented_strict: None,
            measured_declared: None,
        }];
        let json = render_json(&rows);
        assert_eq!(json["summary"]["documented"]["pct"], 100);
        assert!(json["summary"]["documented_strict"].is_null());
        assert!(json["items"][0]["measured_declared"].is_null());
        let text = render_text(&rows);
        assert!(text.contains("documented+:         n/a"), "{text}");
        assert!(text.contains("documented:          1/1 (100%)"), "{text}");
    }

    #[test]
    fn the_summary_counts_over_applicable_rows_and_floors() {
        let rows = rust_rows(&surface(), &set(&["d::sorted"]), &set(&[]));
        let json = render_json(&rows);
        assert_eq!(json["summary"]["documented"]["total"], 6);
        assert_eq!(json["summary"]["documented"]["covered"], 5);
        assert_eq!(json["summary"]["documented"]["pct"], 83);
        assert_eq!(json["summary"]["measured_declared"]["total"], 5);
        assert_eq!(json["summary"]["measured_declared"]["covered"], 1);
        assert_eq!(json["summary"]["measured_declared"]["pct"], 20);
    }
}
