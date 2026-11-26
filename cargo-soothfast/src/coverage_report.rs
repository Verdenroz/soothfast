//! `coverage report`: one row per public item with the dimensions it is
//! covered on. A dimension a tree cannot tell is `n/a`, never 0%.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Value, json};
use soothfast_docs::{claims, markdown, surface::Surface};
use soothfast_report::badges;

use crate::config::{self, Group, Settings};
use crate::coverage::{self, dangling_covers, declared_covers};
use crate::coverage_features;
use crate::coverage_lang;
use crate::invoke::{self, Build, CommonArgs};
use crate::{benchexe, docs, docs_support, workspace};

mod executed;
mod routes;
use executed::{Provenance, Want};

/// A public item and what covers it. `None` is a dimension the item's tree
/// has no way to tell.
#[derive(Default)]
struct Row {
    path: String,
    kind: String,
    documented: bool,
    documented_strict: Option<bool>,
    tested: Option<bool>,
    measured_declared: Option<bool>,
    measured: Option<bool>,
    gated: Option<bool>,
    specified: Option<bool>,
    /// Where the executed columns came from.
    executed: Provenance,
    /// The item's claims were not read: no bench executable was listed, so
    /// `documented_strict` is a lower bound.
    claims_unread: bool,
}

/// Every dimension a report carries: its JSON name and its label.
pub(crate) const DIMENSIONS: [(&str, &str); 7] = [
    ("documented", "documented"),
    ("documented_strict", "documented+"),
    ("tested", "tested"),
    ("measured_declared", "measured (declared)"),
    ("measured", "measured"),
    ("gated", "gated"),
    ("specified", "specified"),
];

/// Whether `name` is a dimension a minimum may be set on.
pub(crate) fn is_dimension(name: &str) -> bool {
    DIMENSIONS.iter().any(|(d, _)| *d == name)
}

impl Row {
    fn dimension(&self, name: &str) -> Option<bool> {
        match name {
            "documented" => Some(self.documented),
            "documented_strict" => self.documented_strict,
            "tested" => self.tested,
            "measured_declared" => self.measured_declared,
            "measured" => self.measured,
            "gated" => self.gated,
            _ => self.specified,
        }
    }
}

/// The line `--check` ends with: a JSON array of every applicable dimension's
/// figures, which a CI record reads back.
pub(crate) const SUMMARY_PREFIX: &str = "soothfast-coverage ";

pub fn run(args: &[String]) -> i32 {
    let mut pkgs: Vec<String> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    let mut lang: Option<String> = None;
    let mut features: Option<String> = None;
    let mut json = false;
    let mut check = false;
    let mut executed = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-p" | "--package" => pkgs.extend(it.next().cloned()),
            "--dir" => dirs.extend(it.next().cloned()),
            "--lang" => lang = it.next().cloned(),
            "--features" => features = it.next().cloned(),
            "--json" => json = true,
            "--check" => check = true,
            "--executed" => executed = true,
            other => {
                eprintln!("soothfast: unknown coverage report arg {other:?}");
                return 2;
            }
        }
    }
    match rows(&pkgs, &dirs, lang.as_deref(), features, check, executed) {
        Ok(rows) => {
            let outside = match check {
                true => Ok(Vec::new()),
                false => pkgs.iter().map(|p| coverage_features::section(p)).collect(),
            };
            let outside: Vec<(String, Value)> = match outside {
                Ok(outside) => outside,
                Err(e) => {
                    eprintln!("soothfast: {e}");
                    return 1;
                }
            };
            if json {
                let mut doc = render_json(&rows);
                doc["features"] = outside.into_iter().map(|(_, json)| json).collect();
                println!("{doc:#}");
            } else if check {
                print!("{}", render_summary(&rows));
            } else {
                print!("{}", render_text(&rows));
                for (text, _) in outside {
                    print!("\n{text}");
                }
            }
            if check {
                held_to_minimums(&rows, &pkgs)
            } else {
                0
            }
        }
        Err((_, e)) if check => could_not_run(&pkgs, &e),
        Err((code, e)) => {
            eprintln!("soothfast: {e}");
            code
        }
    }
}

/// A `--check` whose report could not be built fails only a package that sets
/// a minimum; any other passes with a note in its output.
fn could_not_run(pkgs: &[String], error: &str) -> i32 {
    let settings = config::coverage_defaults(pkgs);
    let code = unbuilt_exit(&settings);
    if code == 0 {
        println!("{}", skip_note(error));
    } else {
        eprintln!("soothfast: {error}");
    }
    code
}

fn unbuilt_exit(settings: &Result<Settings, String>) -> i32 {
    match settings {
        Ok(settings) if !settings.holds_minimum() => 0,
        _ => 2,
    }
}

fn skip_note(error: &str) -> String {
    format!(
        "coverage could not run: {}",
        error.lines().next().unwrap_or_default()
    )
}

/// A dimension's tally and the minimum it must reach, where one is set.
struct Figure {
    name: &'static str,
    label: &'static str,
    covered: u32,
    total: u32,
    min: Option<u32>,
    partial: bool,
}

impl Figure {
    fn pct(&self) -> u32 {
        percent((self.covered, self.total))
    }

    fn short(&self) -> bool {
        self.min.is_some_and(|min| self.pct() < min)
    }
}

fn figures(rows: &[Row], minimums: &Settings) -> Vec<Figure> {
    DIMENSIONS
        .iter()
        .filter_map(|&(name, label)| {
            let (covered, total) = summarize(rows, name)?;
            let min = minimums.minimum(name);
            Some(Figure {
                name,
                label,
                covered,
                total,
                min,
                partial: partial(rows, name),
            })
        })
        .collect()
}

/// Prints the unmet minimums and the summary line. Exits 1 when a minimum is
/// not met and 2 when the configuration cannot be read.
fn held_to_minimums(rows: &[Row], pkgs: &[String]) -> i32 {
    let settings = match config::coverage_defaults(pkgs) {
        Ok(settings) => settings,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    let figures = figures(rows, &settings);
    let summary: Vec<Value> = figures
        .iter()
        .map(|f| {
            json!({
                "dimension": f.name,
                "covered": f.covered,
                "total": f.total,
                "pct": f.pct(),
                "min": f.min,
                "partial": f.partial,
            })
        })
        .collect();
    for f in figures.iter().filter(|f| f.short()) {
        println!(
            "coverage: {} is {}%, below the minimum {}%",
            f.label,
            f.pct(),
            f.min.unwrap_or_default()
        );
    }
    println!("{SUMMARY_PREFIX}{}", Value::Array(summary));
    i32::from(figures.iter().any(Figure::short))
}

type Failure = (i32, String);

fn rows(
    pkgs: &[String],
    dirs: &[String],
    lang: Option<&str>,
    features: Option<String>,
    check: bool,
    executed: bool,
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
            claims_unread: false,
            ..Row::default()
        }));
    }
    if pkgs.is_empty() {
        return Ok(rows);
    }
    let marked = marker_targets(&invoke::project_root());
    for pkg in pkgs {
        let (surf, _, root) =
            docs_support::surface_in(pkg, None, features.as_deref()).map_err(|e| (1, e))?;
        let may_build = !check || needs_bench(pkg).map_err(|e| (2, e))?;
        let build = bench_args(pkg, features.clone(), Group::Docs).map_err(|e| (1, e))?;
        let min_set = check && needs_executed(pkg).map_err(|e| (2, e))?;
        let want = executed_want(check, executed, min_set);
        let columns = executed::gather(pkg, &surf, &root, &build, want).map_err(|e| (1, e))?;
        let handlers = match may_build {
            true => {
                let spec = bench_args(pkg, features.clone(), Group::Spec).map_err(|e| (1, e))?;
                Some(routes::handlers(pkg, &spec).map_err(|e| (1, e))?)
            }
            false => None,
        };
        let decorate = |rows: Vec<Row>| {
            let rows = executed::attach(rows, &columns);
            match &handlers {
                Some(handlers) => routes::attach(rows, &surf, handlers),
                None => rows,
            }
        };
        let records = discover(&build, may_build).map_err(|e| (1, e))?;
        let Some(records) = records else {
            eprintln!(
                "{pkg}: no built bench executable; measured (declared) and gated are n/a and documented+ counts binds only"
            );
            rows.extend(decorate(rust_rows(&surf, None, &marked.items(&[]), None)));
            continue;
        };
        for warning in dangling_covers(&records, &surf, pkg) {
            eprintln!("{warning}");
        }
        let gate_records = gate_discovery(pkg, &build, &records, may_build).map_err(|e| (1, e))?;
        rows.extend(decorate(rust_rows(
            &surf,
            Some(&declared_covers(&records)),
            &marked.items(&records),
            gate_records.as_deref().map(declared_covers).as_ref(),
        )));
    }
    Ok(rows)
}

/// Whether `pkg` has a minimum on a dimension its bench target's claims,
/// declarations and routes decide.
fn needs_bench(pkg: &str) -> Result<bool, String> {
    let minimums = config::coverage_defaults(&[pkg.to_string()])?;
    Ok([
        "documented_strict",
        "measured_declared",
        "gated",
        "specified",
    ]
    .iter()
    .any(|d| minimums.minimum(d).is_some()))
}

/// A `--check` runs the instrumented build only for a package with a minimum
/// on `tested` or `measured`. A plain report runs it on `--executed` and
/// otherwise reads a stored run.
fn executed_want(check: bool, executed: bool, min_set: bool) -> Want {
    match (check, executed) {
        (true, _) if min_set => Want::Run,
        (true, _) => Want::Skip,
        (false, true) => Want::Run,
        (false, false) => Want::Stored,
    }
}

/// Whether `pkg` has a minimum on a dimension an instrumented run decides.
fn needs_executed(pkg: &str) -> Result<bool, String> {
    let minimums = config::coverage_defaults(&[pkg.to_string()])?;
    Ok(["tested", "measured"]
        .iter()
        .any(|d| minimums.minimum(d).is_some()))
}

fn bench_args(pkg: &str, features: Option<String>, group: Group) -> Result<CommonArgs, String> {
    let mut common = CommonArgs {
        pkg: Some(pkg.to_string()),
        features,
        ..CommonArgs::default()
    };
    config::apply(&mut common, group)?;
    Ok(common)
}

/// The registry's items for the bench `common` names, listed by a stored bench
/// executable of either profile when there is one, else by a dev build when
/// `may_build`. No items when its package has no such bench target to link them
/// from, and `None` when it has one that is not built and may not be.
fn discover(common: &CommonArgs, may_build: bool) -> Result<Option<Vec<Value>>, String> {
    let pkg = common.pkg.as_deref().unwrap_or_default();
    let target = common.target.as_deref().unwrap_or("soothfast");
    if !workspace::has_bench_target(pkg, target, None).map_err(|e| e.to_string())? {
        return Ok(Some(Vec::new()));
    }
    let listed = if benchexe::is_stored(common, None, None, Build::Discover) {
        invoke::run_discovery(common, &["--list"])
    } else if benchexe::is_stored(common, None, None, Build::Measure) {
        invoke::run_bench_in(common, &["--list"], None)
    } else if may_build {
        invoke::run_discovery(common, &["--list"])
    } else {
        return Ok(None);
    };
    listed.map(Some).map_err(|e| e.to_string())
}

/// The registry under the gate's features. A second bench build happens only
/// where they differ from the report's.
fn gate_discovery(
    pkg: &str,
    docs: &CommonArgs,
    docs_records: &[Value],
    may_build: bool,
) -> Result<Option<Vec<Value>>, String> {
    let features = config::defaults(&[pkg.to_string()], Group::Gate)?.features;
    let gate = bench_args(pkg, features, Group::Gate)?;
    if (&gate.features, &gate.target) == (&docs.features, &docs.target) {
        return Ok(Some(docs_records.to_vec()));
    }
    discover(&gate, may_build)
}

/// Every item and method of `surf`, with `declared` the items benches cover
/// (`None` when no bench could be listed). `documented_strict` implies `documented`:
/// an example or a marker lifts a documented item, it does not stand in for
/// the docs. `gated` holds the paths the gate's benches declare, and is
/// `None` when they could not be listed.
fn rust_rows(
    surf: &Surface,
    declared: Option<&BTreeSet<String>>,
    marked: &BTreeSet<String>,
    gated: Option<&BTreeSet<String>>,
) -> Vec<Row> {
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
            measured_declared: declared
                .filter(|_| c.info.kind == "function")
                .map(|declared| named(declared, &c.aliases)),
            gated: gated
                .filter(|_| c.info.kind == "function")
                .map(|gated| named(gated, &c.aliases)),
            claims_unread: declared.is_none(),
            ..Row::default()
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
    /// The items markers cover: bound ones, and what a claimed bench `covers`.
    fn items(&self, records: &[Value]) -> BTreeSet<String> {
        self.binds
            .iter()
            .cloned()
            .chain(self.claimed_items(records))
            .collect()
    }

    /// What a claimed bench `covers` (its own id where that is empty).
    fn claimed_items(&self, records: &[Value]) -> BTreeSet<String> {
        coverage::benches(records)
            .filter(|(id, _)| self.claimed.contains(*id))
            .map(|(id, covers)| coverage::measured_path(id, covers))
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

/// Whether `name`'s figure is a lower bound: `documented_strict` when a row's
/// claims were not read, `measured` when the bench run failed.
fn partial(rows: &[Row], name: &str) -> bool {
    name == "documented_strict" && rows.iter().any(|r| r.claims_unread)
        || executed::partial(rows, name)
}

/// The badges for `pkgs`, by file stem: `coverage`, the mean of the percentages
/// of the dimensions that apply, and `coverage-DIMENSION` for each, so one
/// dimension cannot carry the composite unseen.
pub(crate) fn badge_files(
    pkgs: &[String],
    features: Option<&str>,
) -> Result<Vec<(String, Value)>, String> {
    let mut rows = Vec::new();
    for pkg in pkgs {
        let features = features.map(str::to_string);
        rows.extend(
            self::rows(std::slice::from_ref(pkg), &[], None, features, false, false)
                .map_err(|f| f.1)?,
        );
    }
    Ok(badge_set(&rows))
}

fn badge_set(rows: &[Row]) -> Vec<(String, Value)> {
    let pcts: Vec<Option<u32>> = DIMENSIONS
        .iter()
        .map(|(name, _)| summarize(rows, name).map(percent))
        .collect();
    let applicable: Vec<u32> = pcts.iter().flatten().copied().collect();
    let badge = |label: &str, pct: Option<u32>| match pct {
        Some(pct) => badges::coverage_badge(label, pct),
        None => badges::badge(label, "n/a", "lightgrey"),
    };
    let composite =
        (!applicable.is_empty()).then(|| applicable.iter().sum::<u32>() / applicable.len() as u32);
    let mut files = vec![("coverage".to_string(), badge("coverage", composite))];
    for ((name, label), pct) in DIMENSIONS.iter().zip(pcts) {
        let stem = format!("coverage-{name}");
        files.push((stem, badge(&format!("coverage {label}"), pct)));
    }
    files
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
                "tested": r.tested,
                "measured_declared": r.measured_declared,
                "measured": r.measured,
                "gated": r.gated,
                "specified": r.specified,
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
            let value = match value {
                Value::Object(mut figure) if partial(rows, name) => {
                    figure.insert("partial".into(), Value::Bool(true));
                    Value::Object(figure)
                }
                other => other,
            };
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
    let mut out = String::new();
    for (_, label) in DIMENSIONS {
        out.push_str(&format!("{label:<w$} ", w = label.len()));
    }
    out.push_str("item\n");
    for r in rows {
        for (name, label) in DIMENSIONS {
            out.push_str(&format!(
                "{:<w$} ",
                cell(r.dimension(name)),
                w = label.len()
            ));
        }
        out.push_str(&format!("{} ({})\n", r.path, r.kind));
    }
    out.push_str(&render_summary(rows));
    out
}

fn render_summary(rows: &[Row]) -> String {
    let mut out = String::new();
    for (name, label) in DIMENSIONS {
        let told = summarize(rows, name);
        let note = match (name, told) {
            ("documented_strict", Some(_)) if partial(rows, name) => {
                Some("claims not read: no bench executable".to_string())
            }
            ("tested" | "measured", told) => executed::note(rows, name, told.is_some()),
            _ => None,
        };
        let figure = match (told, note) {
            (Some(s), Some(n)) => format!("{}/{} ({}%, {n})", s.0, s.1, percent(s)),
            (Some(s), None) => format!("{}/{} ({}%)", s.0, s.1, percent(s)),
            (None, Some(n)) => format!("n/a ({n})"),
            (None, None) => "n/a".to_string(),
        };
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
        let rows = rust_rows(
            &surface(),
            Some(&set(&[])),
            &set(&["d::alias::bound"]),
            Some(&set(&[])),
        );
        assert_eq!(row(&rows, "d::sorted").documented_strict, Some(true));
        assert_eq!(row(&rows, "d::bound").documented_strict, Some(true));
        assert_eq!(row(&rows, "d::bare").documented_strict, Some(false));
    }

    #[test]
    fn a_marker_does_not_stand_in_for_the_docs() {
        let rows = rust_rows(
            &surface(),
            Some(&set(&[])),
            &set(&["d::blank"]),
            Some(&set(&[])),
        );
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
            Some(&set(&["d::alias::sorted", "d::Thing::go"])),
            &set(&[]),
            Some(&set(&[])),
        );
        assert_eq!(row(&rows, "d::sorted").measured_declared, Some(true));
        assert_eq!(row(&rows, "d::Thing::go").measured_declared, Some(true));
        assert_eq!(row(&rows, "d::bare").measured_declared, Some(false));
        assert_eq!(row(&rows, "d::Thing").measured_declared, None);
    }

    #[test]
    fn without_a_listed_bench_measured_is_not_applicable() {
        let rows = rust_rows(&surface(), None, &set(&["d::alias::bound"]), None);
        assert!(rows.iter().all(|r| r.measured_declared.is_none()));
        assert_eq!(row(&rows, "d::bound").documented_strict, Some(true));
    }

    #[test]
    fn an_unread_bench_marks_documented_plus_as_a_lower_bound() {
        let rows = rust_rows(&surface(), None, &set(&[]), None);
        let text = render_text(&rows);
        assert!(
            text.contains("documented+:         1/6 (16%, claims not read: no bench executable)"),
            "{text}"
        );
        let json = render_json(&rows);
        assert_eq!(json["summary"]["documented_strict"]["partial"], true);
        assert!(json["summary"]["documented"].get("partial").is_none());
        let read = render_json(&rust_rows(
            &surface(),
            Some(&set(&[])),
            &set(&[]),
            Some(&set(&[])),
        ));
        assert!(
            read["summary"]["documented_strict"]
                .get("partial")
                .is_none()
        );
        assert!(
            figures(&rows, &Settings::default())
                .iter()
                .any(|f| f.partial)
        );
    }

    #[test]
    fn the_composite_badge_is_the_mean_of_the_applicable_dimensions() {
        let rows = rust_rows(
            &surface(),
            Some(&set(&["d::sorted"])),
            &set(&[]),
            Some(&set(&[])),
        );
        let files = badge_set(&rows);
        let message = |stem: &str| {
            let (_, badge) = files.iter().find(|(s, _)| s == stem).unwrap();
            badge["message"].as_str().unwrap().to_string()
        };
        assert_eq!(message("coverage-documented"), "83%");
        assert_eq!(message("coverage-documented_strict"), "16%");
        assert_eq!(message("coverage-measured_declared"), "20%");
        assert_eq!(message("coverage-gated"), "0%");
        assert_eq!(message("coverage"), "29%");
    }

    #[test]
    fn a_dimension_no_row_can_tell_has_a_grey_badge_and_no_vote() {
        let rows = vec![Row {
            path: "src/a.py#f".into(),
            kind: "function".into(),
            documented: true,
            ..Row::default()
        }];
        let files = badge_set(&rows);
        assert_eq!(files[0].1["message"], "100%");
        let strict = files
            .iter()
            .find(|(s, _)| s == "coverage-documented_strict");
        assert_eq!(strict.unwrap().1["message"], "n/a");
        assert!(badge_set(&[]).iter().all(|(_, b)| b["message"] == "n/a"));
    }

    #[test]
    fn only_callables_a_gate_bench_covers_are_gated() {
        let rows = rust_rows(
            &surface(),
            Some(&set(&[])),
            &set(&[]),
            Some(&set(&["d::alias::sorted", "d::Thing::go", "d::Thing"])),
        );
        assert_eq!(row(&rows, "d::sorted").gated, Some(true));
        assert_eq!(row(&rows, "d::Thing::go").gated, Some(true));
        assert_eq!(row(&rows, "d::bare").gated, Some(false));
        assert_eq!(row(&rows, "d::Thing").gated, None);
    }

    #[test]
    fn a_claimed_bench_covers_what_it_measures_not_the_bench() {
        let md = "<!-- soothfast:claim demo::bench_run.alloc.allocs <= 8 -->\n";
        let markers = targets_in([md.to_string()].into_iter());
        let records = [
            json!({ "type": "item", "id": "demo::bench_run", "covers": "demo::run" }),
            json!({ "type": "item", "id": "demo::bench_idle", "covers": "demo::idle" }),
        ];
        assert_eq!(markers.claimed_items(&records), set(&["demo::run"]));
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
        let rows = rust_rows(
            &surf,
            Some(&set(&[])),
            &markers.items(&records),
            Some(&set(&[])),
        );
        assert_eq!(row(&rows, "demo::run").documented_strict, Some(true));
    }

    #[test]
    fn a_dimension_no_row_can_tell_is_not_applicable() {
        let rows = vec![Row {
            path: "src/a.py#f".into(),
            kind: "function".into(),
            documented: true,
            ..Row::default()
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
    fn a_figure_is_short_only_below_a_minimum_that_is_set() {
        let rows = rust_rows(&surface(), Some(&set(&[])), &set(&[]), Some(&set(&[])));
        let settings = |documented: Option<u32>, strict: Option<u32>| Settings {
            coverage: documented,
            minimums: strict
                .map(|n| ("documented_strict".to_string(), n))
                .into_iter()
                .collect(),
            ..Settings::default()
        };
        let short = |s: Settings| {
            figures(&rows, &s)
                .iter()
                .filter(|f| f.short())
                .map(|f| f.name)
                .collect::<Vec<_>>()
        };
        assert!(short(settings(None, None)).is_empty());
        assert!(short(settings(Some(83), Some(16))).is_empty());
        assert_eq!(short(settings(Some(84), None)), ["documented"]);
        assert_eq!(short(settings(None, Some(17))), ["documented_strict"]);
    }

    #[test]
    fn the_instrumented_run_happens_on_a_flag_or_a_minimum() {
        assert!(executed_want(false, true, false) == Want::Run);
        assert!(executed_want(false, false, false) == Want::Stored);
        assert!(executed_want(true, false, true) == Want::Run);
        assert!(executed_want(true, false, false) == Want::Skip);
    }

    #[test]
    fn an_unbuilt_report_fails_only_a_package_with_a_minimum() {
        let with = |min: Settings| unbuilt_exit(&Ok(min));
        assert_eq!(with(Settings::default()), 0);
        let documented = Settings {
            coverage: Some(80),
            ..Settings::default()
        };
        assert_eq!(with(documented), 2);
        let other = Settings {
            minimums: [("gated".to_string(), 10)].into(),
            ..Settings::default()
        };
        assert_eq!(with(other), 2);
        assert_eq!(unbuilt_exit(&Err("bad config".into())), 2);
    }

    #[test]
    fn the_skip_note_carries_the_first_error_line() {
        assert_eq!(
            skip_note("rustdoc failed\n  --> src/lib.rs"),
            "coverage could not run: rustdoc failed"
        );
    }

    #[test]
    fn a_dimension_that_does_not_apply_has_no_figure() {
        let rows = vec![Row {
            path: "src/a.py#f".into(),
            kind: "function".into(),
            documented: true,
            ..Row::default()
        }];
        let settings = Settings {
            minimums: [("measured_declared".to_string(), 90)].into(),
            ..Settings::default()
        };
        let got = figures(&rows, &settings);
        assert_eq!(
            got.iter().map(|f| f.name).collect::<Vec<_>>(),
            ["documented"]
        );
    }

    #[test]
    fn the_summary_counts_over_applicable_rows_and_floors() {
        let rows = rust_rows(
            &surface(),
            Some(&set(&["d::sorted"])),
            &set(&[]),
            Some(&set(&[])),
        );
        let json = render_json(&rows);
        assert_eq!(json["summary"]["documented"]["total"], 6);
        assert_eq!(json["summary"]["documented"]["covered"], 5);
        assert_eq!(json["summary"]["documented"]["pct"], 83);
        assert_eq!(json["summary"]["measured_declared"]["total"], 5);
        assert_eq!(json["summary"]["measured_declared"]["covered"], 1);
        assert_eq!(json["summary"]["measured_declared"]["pct"], 20);
    }
}
