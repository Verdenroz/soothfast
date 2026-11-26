//! `coverage measure` / `coverage docs`: report public items lacking a
//! measurement binding or documentation. Report-and-nudge by default;
//! `coverage docs --min PCT` optionally gates, `--badge FILE` emits a
//! shields.io endpoint badge so the number is published, never asserted.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;
use soothfast_docs::surface::Surface;

use crate::config::{self, Group};
use crate::coverage_lang::{self, Target};
use crate::invoke::{self, CommonArgs};

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("measure") => measure(&args[1..]),
        Some("docs") => docs(&args[1..]),
        _ => {
            eprintln!(
                "soothfast: usage: cargo soothfast coverage measure|docs -p PKG [-p PKG ...] [--min PCT] [--badge FILE]\n       coverage docs --dir D --lang L [--min PCT] [--badge FILE]"
            );
            2
        }
    }
}

fn measure(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut badges: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--badge" {
            badges.extend(it.next().cloned());
        } else if !common.try_parse(a, &mut it) {
            eprintln!("soothfast: unknown coverage arg {a:?}");
            return 2;
        }
    }
    let Some(pkg) = common.pkg.clone() else {
        eprintln!("soothfast: coverage measure requires -p PKG");
        return 2;
    };
    if let Err(e) = config::apply(&mut common, Group::Docs) {
        eprintln!("soothfast: {e}");
        return 2;
    }

    // What is measured: registry list, attributing `covers` where set.
    let records = match invoke::run_discovery(&common, &["--list"]) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 1;
        }
    };
    let covered: BTreeSet<String> = records
        .iter()
        .filter(|r| r["type"] == "item")
        .filter_map(|r| {
            let covers = r["covers"].as_str().unwrap_or("");
            let id = r["id"].as_str()?;
            Some(if covers.is_empty() { id } else { covers }.to_string())
        })
        .collect();

    let surf = match load_surface(&pkg, common.features.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 1;
        }
    };
    for warning in dangling_covers(&records, &surf, &pkg) {
        eprintln!("{warning}");
    }
    let mut missing = 0u32;
    let callables: Vec<_> = surf
        .counted()
        .into_iter()
        .filter(|c| c.info.kind == "function")
        .collect();
    for c in &callables {
        let is_measured = c.aliases.iter().any(|a| covered.contains(a.as_str()));
        let mark = if is_measured {
            "measured  "
        } else {
            "UNMEASURED"
        };
        if !is_measured {
            missing += 1;
        }
        println!("{mark} {}", c.path);
    }
    let p = report("measured", callables.len() as u32, missing);
    for file in &badges {
        if let Err(e) = write_badge(Path::new(file), "soothfast measured", p) {
            eprintln!("soothfast: {e}");
            return 1;
        }
    }
    0
}

fn docs(args: &[String]) -> i32 {
    // Repeatable `-p`: several crates aggregate into one figure, so a badge
    // can speak for the whole published surface rather than one crate.
    let mut pkgs: Vec<String> = Vec::new();
    let mut min: Option<u32> = None;
    let mut badges: Vec<String> = Vec::new();
    let mut features: Option<String> = None;
    let mut dirs: Vec<String> = Vec::new();
    let mut lang: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-p" | "--package" => pkgs.extend(it.next().cloned()),
            "--dir" => dirs.extend(it.next().cloned()),
            "--lang" => lang = it.next().cloned(),
            "--min" => min = it.next().and_then(|v| v.parse().ok()),
            "--badge" => badges.extend(it.next().cloned()),
            "--features" => features = it.next().cloned(),
            other => {
                eprintln!("soothfast: unknown coverage docs arg {other:?}");
                return 2;
            }
        }
    }
    let (features, min) = match config::defaults(&pkgs, Group::Docs) {
        Ok(defaults) => (features.or(defaults.features), min.or(defaults.coverage)),
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    let targets = match lang_targets(&pkgs, &dirs, lang.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    let mut total = 0u32;
    let mut missing = 0u32;
    for target in &targets {
        let entries = match coverage_lang::entries(target) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("soothfast: {e}");
                return 1;
            }
        };
        for entry in &entries {
            let mark = if entry.documented {
                "documented  "
            } else {
                missing += 1;
                "UNDOCUMENTED"
            };
            println!("{mark} {} ({})", entry.id, entry.kind);
        }
        total += entries.len() as u32;
    }
    for pkg in &pkgs {
        let surf = match load_surface(pkg, features.as_deref()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("soothfast: {e}");
                return 1;
            }
        };
        let counted = surf.counted();
        for c in &counted {
            let mark = if c.info.has_docs {
                "documented  "
            } else {
                "UNDOCUMENTED"
            };
            if !c.info.has_docs {
                missing += 1;
            }
            println!("{mark} {} ({})", c.path, c.info.kind);
        }
        total += counted.len() as u32;
    }
    let p = report("documented", total, missing);
    for file in &badges {
        if let Err(e) = write_badge(Path::new(file), "soothfast docs", p) {
            eprintln!("soothfast: {e}");
            return 1;
        }
    }
    if let Some(min) = min
        && p < min
    {
        println!("coverage docs: FAILED ({p}% < required {min}%)");
        return 1;
    }
    0
}

/// A warning per bench whose `covers` names no item or method of `surf`.
pub(crate) fn dangling_covers(records: &[Value], surf: &Surface, pkg: &str) -> Vec<String> {
    records
        .iter()
        .filter(|r| r["type"] == "item")
        .filter_map(|r| Some((r["id"].as_str()?, r["covers"].as_str()?)))
        .filter(|(_, covers)| !covers.is_empty() && surf.lookup(covers).is_none())
        .map(|(id, covers)| {
            format!("warning: bench {id} covers {covers}, which is not a public item of {pkg}")
        })
        .collect()
}

/// Source trees to cover besides the `-p` crates: `--dir`/`--lang`, else the
/// manifest's non-Rust projects when no crate was named.
fn lang_targets(
    pkgs: &[String],
    dirs: &[String],
    lang: Option<&str>,
) -> Result<Vec<Target>, String> {
    if dirs.is_empty() && lang.is_none() {
        if !pkgs.is_empty() {
            return Ok(Vec::new());
        }
        let targets = coverage_lang::targets(Path::new("."), dirs, lang)?;
        if targets.is_empty() {
            return Err("coverage docs requires -p PKG, --dir D --lang L, or [[project]] entries in soothfast.toml".into());
        }
        return Ok(targets);
    }
    coverage_lang::targets(Path::new("."), dirs, lang)
}

fn load_surface(
    pkg: &str,
    features: Option<&str>,
) -> Result<soothfast_docs::surface::Surface, String> {
    crate::docs_support::surface_in(pkg, None, features).map(|(surface, _, _)| surface)
}

fn report(what: &str, total: u32, missing: u32) -> u32 {
    let p = pct(total, missing);
    println!(
        "coverage: {}/{} public item(s) {what} ({p}%)",
        total - missing,
        total
    );
    p
}

/// Integer floor percentage; an empty surface counts as fully covered.
fn pct(total: u32, missing: u32) -> u32 {
    let covered = total.saturating_sub(missing);
    covered
        .saturating_mul(100)
        .checked_div(total)
        .unwrap_or(100)
}

/// Badge at `path`, creating parent directories: a `.svg` path renders the
/// self-hosted SVG, anything else the shields.io endpoint JSON. The message
/// is always the measured figure — a badge is a claim like any other.
fn write_badge(path: &Path, label: &str, pct: u32) -> Result<(), String> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let value = soothfast_report::badges::coverage_badge(label, pct);
    let content = if path.extension().is_some_and(|e| e == "svg") {
        soothfast_report::badges::svg_from(&value)
    } else {
        value.to_string()
    };
    std::fs::write(path, content).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    println!("coverage: wrote {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use soothfast_docs::surface::{ItemInfo, Surface};

    use super::{dangling_covers, pct, write_badge};

    fn surface() -> Surface {
        let info = |kind: &str| ItemInfo {
            kind: kind.into(),
            fingerprint: 1,
            signature: String::new(),
            has_docs: true,
        };
        let mut s = Surface::default();
        s.items.insert("demo::run".into(), info("function"));
        s.members.insert("demo::Thing::go".into(), info("function"));
        s
    }

    #[test]
    fn a_covers_naming_nothing_is_warned_about() {
        let records = [
            json!({ "type": "item", "id": "demo::bench_run", "covers": "demo::run" }),
            json!({ "type": "item", "id": "demo::bench_go", "covers": "demo::Thing::go" }),
            json!({ "type": "item", "id": "demo::bench_old", "covers": "demo::gone" }),
            json!({ "type": "item", "id": "demo::bench_plain", "covers": "" }),
            json!({ "type": "result", "id": "demo::bench_old", "covers": "demo::other" }),
        ];
        assert_eq!(
            dangling_covers(&records, &surface(), "demo"),
            [
                "warning: bench demo::bench_old covers demo::gone, which is not a public item of demo"
            ]
        );
    }

    #[test]
    fn pct_floors_and_treats_empty_as_full() {
        assert_eq!(pct(0, 0), 100);
        assert_eq!(pct(3, 0), 100);
        assert_eq!(pct(3, 1), 66);
        assert_eq!(pct(123, 15), 87);
    }

    #[test]
    fn badge_file_is_shields_endpoint_json() {
        let path =
            std::env::temp_dir().join(format!("soothfast-badge-{}.json", std::process::id()));
        write_badge(&path, "soothfast docs", 100).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["schemaVersion"], 1);
        assert_eq!(v["label"], "soothfast docs");
        assert_eq!(v["message"], "100%");
        assert_eq!(v["color"], "brightgreen");
        let _ = std::fs::remove_file(&path);
    }
}
