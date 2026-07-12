//! `coverage measure` / `coverage docs`: report public items lacking a
//! measurement binding or documentation. Report-and-nudge by default;
//! `coverage docs --min PCT` optionally gates, `--badge FILE` emits a
//! shields.io endpoint badge so the number is published, never asserted.

use std::collections::BTreeSet;
use std::path::Path;

use crate::invoke::{self, CommonArgs};

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("measure") => measure(&args[1..]),
        Some("docs") => docs(&args[1..]),
        _ => {
            eprintln!(
                "soothfast: usage: cargo soothfast coverage measure|docs -p PKG [-p PKG ...] [--min PCT] [--badge FILE]"
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

    // What is measured: registry list, attributing `covers` where set.
    let records = match invoke::run_bench(&common, &["--list"]) {
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
    let mut missing = 0u32;
    let groups: Vec<_> = surf
        .grouped()
        .into_iter()
        .filter(|g| surf.items[g.representative].kind == "function")
        .collect();
    for g in &groups {
        let is_measured = g.aliases.iter().any(|a| covered.contains(a.as_str()));
        let mark = if is_measured {
            "measured  "
        } else {
            "UNMEASURED"
        };
        if !is_measured {
            missing += 1;
        }
        println!("{mark} {}", g.representative);
    }
    let p = report("measured", groups.len() as u32, missing);
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
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-p" | "--package" => pkgs.extend(it.next().cloned()),
            "--min" => min = it.next().and_then(|v| v.parse().ok()),
            "--badge" => badges.extend(it.next().cloned()),
            "--features" => features = it.next().cloned(),
            other => {
                eprintln!("soothfast: unknown coverage docs arg {other:?}");
                return 2;
            }
        }
    }
    if pkgs.is_empty() {
        eprintln!("soothfast: coverage docs requires -p PKG");
        return 2;
    }
    let mut total = 0u32;
    let mut missing = 0u32;
    for pkg in &pkgs {
        let surf = match load_surface(pkg, features.as_deref()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("soothfast: {e}");
                return 1;
            }
        };
        let groups = surf.grouped();
        for g in &groups {
            let info = &surf.items[g.representative];
            let mark = if info.has_docs {
                "documented  "
            } else {
                "UNDOCUMENTED"
            };
            if !info.has_docs {
                missing += 1;
            }
            println!("{mark} {} ({})", g.representative, info.kind);
        }
        total += groups.len() as u32;
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

fn load_surface(
    pkg: &str,
    features: Option<&str>,
) -> Result<soothfast_docs::surface::Surface, String> {
    let (doc, root) = invoke::rustdoc_json_in(pkg, None, features, invoke::Visibility::Public)
        .map_err(|e| e.to_string())?;
    Ok(soothfast_docs::surface::from_rustdoc(&doc, &root))
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
    use super::{pct, write_badge};

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
