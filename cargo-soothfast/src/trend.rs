//! Trend time series: one JSONL record per `trend append` (typically per
//! merge to main), rendered as first→last drift per item. Chart rendering
//! arrives with soothfast-report in Phase 4; the series format is the contract.

use serde_json::{Value, json};

use crate::invoke::{self, CommonArgs};

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("append") => append(&args[1..]),
        Some("render") => render(),
        _ => {
            eprintln!("soothfast: usage: cargo soothfast trend append|render [-p PKG]");
            2
        }
    }
}

fn trend_path() -> std::io::Result<std::path::PathBuf> {
    Ok(invoke::workspace_root()?
        .join(".soothfast")
        .join("trend.jsonl"))
}

fn append(args: &[String]) -> i32 {
    let mut common = CommonArgs::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if !common.try_parse(a, &mut it) {
            eprintln!("soothfast: unknown trend arg {a:?}");
            return 2;
        }
    }
    let records = match invoke::run_bench(&common, &[]) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 1;
        }
    };
    let run = invoke::collect(&records);

    let commit =
        invoke::git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|_| "unknown".into());
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let record = json!({
        "unix": unix,
        "commit": commit,
        "noise_pct": run.noise_pct,
        "items": invoke::run_to_items_value(&run),
    });

    let path = match trend_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 1;
        }
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut line = record.to_string();
    line.push('\n');
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if std::fs::write(&path, existing + &line).is_err() {
        eprintln!("soothfast: failed to write {}", path.display());
        return 1;
    }
    println!(
        "trend: appended {} item(s) at commit {commit} -> {}",
        run.items.len(),
        path.display()
    );
    0
}

fn render() -> i32 {
    let path = match trend_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 1;
        }
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("soothfast: no trend series yet — run `cargo soothfast trend append` first");
        return 2;
    };
    let points: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if points.is_empty() {
        eprintln!("soothfast: trend series is empty");
        return 2;
    }

    println!(
        "trend: {} point(s), {} -> {}",
        points.len(),
        points[0]["commit"].as_str().unwrap_or("?"),
        points[points.len() - 1]["commit"].as_str().unwrap_or("?"),
    );
    // Per item+metric: first -> last with relative drift.
    let mut ids: Vec<String> = points
        .iter()
        .flat_map(|p| {
            p["items"]
                .as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>())
        })
        .flatten()
        .collect();
    ids.sort();
    ids.dedup();

    for id in &ids {
        for (metric, path_keys) in [
            ("instructions", ["perfcnt", "instructions"]),
            ("walltime_median_ns", ["walltime", "median_ns"]),
            ("allocs", ["alloc", "allocs"]),
            ("size_bytes", ["buildcost", "size_bytes"]),
        ] {
            let series: Vec<f64> = points
                .iter()
                .filter_map(|p| p["items"][id][path_keys[0]][path_keys[1]].as_f64())
                .collect();
            if series.len() < 2 {
                continue;
            }
            let (first, last) = (series[0], series[series.len() - 1]);
            if first <= 0.0 {
                continue;
            }
            let drift = (last - first) / first * 100.0;
            println!(
                "{id:<44} {metric:<20} {first:.1} -> {last:.1} ({drift:+.1}% over {} pts)",
                series.len()
            );
        }
    }
    0
}
