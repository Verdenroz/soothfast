//! Trend time series: one JSONL record per `trend append` (typically per
//! merge to main), rendered as first→last drift per item. Chart rendering
//! lives in `soothfast-report`; the series format is the contract.

use serde_json::{Value, json};

use crate::invoke::{self, CommonArgs};

pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("append") => append(&args[1..]),
        Some("render") => render(),
        _ => {
            eprintln!(
                "soothfast: usage: cargo soothfast trend append|render [-p PKG] [--from-baseline NAME]"
            );
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
    let mut from_baseline: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if common.try_parse(a, &mut it) {
            continue;
        }
        match a.as_str() {
            "--from-baseline" => match it.next() {
                Some(n) => from_baseline = Some(n.clone()),
                None => {
                    eprintln!("soothfast: --from-baseline needs a name");
                    return 2;
                }
            },
            other => {
                eprintln!("soothfast: unknown trend arg {other:?}");
                return 2;
            }
        }
    }

    let commit =
        invoke::git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|_| "unknown".into());
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // A saved baseline already carries every backend's metrics; reading it
    // spares the pipeline a full re-measurement of the same commit.
    let (record, count) = if let Some(name) = &from_baseline {
        let doc = match invoke::load_baseline(name) {
            Ok(Some(d)) => d,
            Ok(None) => {
                eprintln!("soothfast: no baseline named {name:?}");
                return 2;
            }
            Err(e) => {
                eprintln!("soothfast: failed to load baseline: {e}");
                return 1;
            }
        };
        match point_from_baseline(&doc, common.pkg.as_deref(), &commit, unix) {
            Ok(r) => {
                let count = r["items"].as_object().map(|o| o.len()).unwrap_or(0);
                (r, count)
            }
            Err(e) => {
                eprintln!("soothfast: baseline {name:?}: {e}");
                return 2;
            }
        }
    } else {
        let records = match invoke::run_bench(&common, &[]) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("soothfast: {e}");
                return 1;
            }
        };
        let run = invoke::collect(&records);
        let record = json!({
            "unix": unix,
            "commit": commit,
            "noise_pct": run.noise_pct,
            "items": invoke::run_to_items_value(&run),
        });
        (record, run.items.len())
    };

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
        "trend: appended {count} item(s) at commit {commit} -> {}",
        path.display()
    );
    0
}

/// A trend point built from a saved baseline instead of a fresh measurement.
/// Buildcost pseudo-items and other packages' entries are dropped so the
/// point holds exactly what `trend append -p PKG` would have measured.
fn point_from_baseline(
    doc: &Value,
    pkg: Option<&str>,
    commit: &str,
    unix: u64,
) -> Result<Value, String> {
    let items: serde_json::Map<String, Value> = doc["items"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|(id, _)| !id.starts_with("buildcost::"))
        .filter(|(id, _)| pkg.is_none_or(|p| invoke::id_pkg(id) == p.replace('-', "_").as_str()))
        .collect();
    if items.is_empty() {
        return Err(format!(
            "no bench items{} — measure into it first",
            pkg.map(|p| format!(" for package {p:?}"))
                .unwrap_or_default()
        ));
    }
    Ok(json!({
        "unix": unix,
        "commit": commit,
        "noise_pct": doc["noise_pct"],
        "items": items,
    }))
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

#[cfg(test)]
mod tests {
    use super::point_from_baseline;
    use serde_json::json;

    fn baseline() -> serde_json::Value {
        json!({
            "version": 1,
            "noise_pct": 0.42,
            "items": {
                "finance_query::indicators::sma": {
                    "fingerprint": "abc",
                    "perfcnt": { "instructions": 1000, "cycles": 900, "cache_refs": 10 },
                    "walltime": { "median_ns": 50.0, "mad_ns": 1.0, "p99_ns": 70.0 },
                    "alloc": { "allocs": 2, "bytes": 128 }
                },
                "other_pkg::thing": { "walltime": { "median_ns": 9.0 } },
                "buildcost::finance-query::default": {
                    "buildcost": { "build_ms": 12000, "size_bytes": 4096 }
                }
            }
        })
    }

    #[test]
    fn a_point_carries_the_baselines_metrics_verbatim() {
        let p = point_from_baseline(&baseline(), Some("finance-query"), "abc1234", 1700).unwrap();
        assert_eq!(p["unix"], 1700);
        assert_eq!(p["commit"], "abc1234");
        assert_eq!(p["noise_pct"], 0.42);
        let item = &p["items"]["finance_query::indicators::sma"];
        assert_eq!(item["perfcnt"]["instructions"], 1000);
        assert_eq!(item["walltime"]["median_ns"], 50.0);
        assert_eq!(item["alloc"]["allocs"], 2);
    }

    #[test]
    fn out_of_scope_entries_are_dropped() {
        // `-p` scopes the point the way it scopes a measured run: no other
        // packages, no buildcost pseudo-items.
        let p = point_from_baseline(&baseline(), Some("finance-query"), "c", 0).unwrap();
        let items = p["items"].as_object().unwrap();
        assert_eq!(items.len(), 1);
        assert!(items.contains_key("finance_query::indicators::sma"));
    }

    #[test]
    fn no_package_keeps_every_bench_item_but_never_buildcost() {
        let p = point_from_baseline(&baseline(), None, "c", 0).unwrap();
        let items = p["items"].as_object().unwrap();
        assert_eq!(items.len(), 2);
        assert!(!items.contains_key("buildcost::finance-query::default"));
    }

    #[test]
    fn an_empty_slice_is_an_error_not_an_empty_point() {
        let err = point_from_baseline(&baseline(), Some("absent-pkg"), "c", 0).unwrap_err();
        assert!(err.contains("absent-pkg"), "unhelpful error: {err}");
    }
}
