//! `Run`: one measurement run keyed by item id, collected from records and
//! round-tripped through its saved form.

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// One item's collected metrics across backends.
#[derive(Default, Debug, Clone)]
pub struct ItemMetrics {
    pub fingerprint: String,
    pub covers: String,
    pub median_ns: Option<f64>,
    pub mad_ns: Option<f64>,
    pub p99_ns: Option<f64>,
    pub instructions: Option<u64>,
    pub cycles: Option<u64>,
    pub cache_refs: Option<u64>,
    pub ir: Option<u64>,
    /// Iterations `ir` averages over, when the harness says.
    pub window: Option<u64>,
    pub allocs: Option<u64>,
    pub bytes: Option<u64>,
    pub polls: Option<u64>,
    pub wakes: Option<u64>,
    /// Gate threshold widened by `#[bench(tolerance = "...")]`.
    pub tolerance_pct: Option<f64>,
    /// Per-round walltime medians when a side ran interleaved rounds.
    pub wall_rounds: Vec<f64>,
    /// buildcost pseudo-items
    pub build_ms: Option<u64>,
    pub size_bytes: Option<u64>,
    /// Set when this item alone came from a stored measurement.
    pub reused: Option<Reused>,
    /// Id prefix of the `import` that produced this item.
    pub imported: Option<String>,
}

/// One assertion verdict from the runner.
#[derive(Debug, Clone, PartialEq)]
pub struct AssertionOutcome {
    pub id: String,
    pub kind: String,
    pub ok: bool,
    pub detail: String,
}

impl AssertionOutcome {
    /// The verdict with its item id, as a baseline or a stored run keeps it.
    pub fn to_json(&self) -> Value {
        json!({ "id": self.id, "kind": self.kind, "ok": self.ok, "detail": self.detail })
    }

    /// The verdict without its item id, as a per-item store keeps it.
    pub fn verdict_json(&self) -> Value {
        json!({ "kind": self.kind, "ok": self.ok, "detail": self.detail })
    }

    /// A verdict of item `id` read back from [`AssertionOutcome::verdict_json`].
    pub fn from_verdict_json(id: &str, v: &Value) -> Option<AssertionOutcome> {
        Some(AssertionOutcome {
            id: id.to_string(),
            kind: v["kind"].as_str()?.to_string(),
            ok: v["ok"].as_bool()?,
            detail: v["detail"].as_str()?.to_string(),
        })
    }
}

/// An `[object, symbol]` pair of an `executed` record.
pub fn pair(v: &Value) -> Option<(String, String)> {
    Some((v[0].as_str()?.to_string(), v[1].as_str()?.to_string()))
}

/// A full measurement run, keyed by item ID.
#[derive(Default, Debug)]
pub struct Run {
    pub noise_pct: Option<f64>,
    pub build: Option<crate::buildstamp::BuildStamp>,
    pub gating_backend: Option<String>,
    pub items: BTreeMap<String, ItemMetrics>,
    pub assertions: Vec<AssertionOutcome>,
    /// The `(object, symbol)` pairs each item executed, from a callgrind run.
    pub executed: BTreeMap<String, Vec<(String, String)>>,
    /// Set when some metrics came from a stored run instead of this process.
    pub reused: Option<Reused>,
}

/// Which metrics of a run were taken from a stored run, and what it measured.
#[derive(Debug, Clone, PartialEq)]
pub struct Reused {
    pub from: String,
    pub metrics: Vec<&'static str>,
}

/// Fold raw runner records into per-item metrics.
pub fn collect(records: &[Value]) -> Run {
    let mut run = Run::default();
    for rec in records {
        match rec["type"].as_str() {
            Some("calibration") => run.noise_pct = rec["noise_pct"].as_f64(),
            Some("env") => run.gating_backend = rec["gating_backend"].as_str().map(str::to_string),
            Some("assertion") => run.assertions.push(AssertionOutcome {
                id: rec["id"].as_str().unwrap_or("").to_string(),
                kind: rec["kind"].as_str().unwrap_or("").to_string(),
                ok: rec["ok"].as_bool().unwrap_or(false),
                detail: rec["detail"].as_str().unwrap_or("").to_string(),
            }),
            Some("executed") if run.gating_backend.as_deref() == Some("callgrind") => {
                if let Some(id) = rec["id"].as_str() {
                    let functions = rec["functions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(pair)
                        .collect();
                    run.executed.insert(id.to_string(), functions);
                }
            }
            Some("result") => {
                let Some(id) = rec["id"].as_str() else {
                    continue;
                };
                let item = run.items.entry(id.to_string()).or_default();
                if let Some(fp) = rec["fingerprint"].as_str() {
                    item.fingerprint = fp.to_string();
                }
                if let Some(c) = rec["covers"].as_str() {
                    item.covers = c.to_string();
                }
                if let Some(t) = rec["tolerance_pct"].as_f64() {
                    item.tolerance_pct = Some(t);
                }
                let m = &rec["metrics"];
                match rec["backend"].as_str() {
                    Some("walltime") => {
                        item.median_ns = m["median_ns"].as_f64();
                        item.mad_ns = m["mad_ns"].as_f64();
                        item.p99_ns = m["p99_ns"].as_f64();
                    }
                    Some("perfcnt") => {
                        item.instructions = m["instructions"].as_u64();
                        item.cycles = m["cycles"].as_u64();
                        item.cache_refs = m["cache_refs"].as_u64();
                    }
                    Some("callgrind") => {
                        item.ir = m["ir"].as_u64();
                        item.window = m["window"].as_u64();
                    }
                    Some("alloc") => {
                        item.allocs = m["allocs"].as_u64();
                        item.bytes = m["bytes"].as_u64();
                    }
                    Some("asyncexec") => {
                        item.polls = m["polls"].as_u64();
                        item.wakes = m["wakes"].as_u64();
                    }
                    Some("buildcost") => {
                        item.build_ms = m["build_ms"].as_u64();
                        item.size_bytes = m["size_bytes"].as_u64();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    run
}

/// Serialize a run into the baseline `items` shape.
pub fn run_to_items_value(run: &Run) -> Value {
    let mut items = serde_json::Map::new();
    for (id, m) in &run.items {
        let mut entry = json!({ "fingerprint": m.fingerprint, "covers": m.covers });
        if let Some(t) = m.tolerance_pct {
            entry["tolerance_pct"] = json!(t);
        }
        if let Some(v) = m.median_ns {
            entry["walltime"] = json!({
                "median_ns": v,
                "mad_ns": m.mad_ns.unwrap_or(0.0),
                "p99_ns": m.p99_ns.unwrap_or(0.0),
            });
            if !m.wall_rounds.is_empty() {
                entry["walltime"]["rounds"] = json!(m.wall_rounds);
            }
        }
        if let Some(i) = m.instructions {
            entry["perfcnt"] = json!({
                "instructions": i,
                "cycles": m.cycles.unwrap_or(0),
                "cache_refs": m.cache_refs.unwrap_or(0),
            });
        }
        if let Some(ir) = m.ir {
            entry["callgrind"] = json!({ "ir": ir });
            if let Some(window) = m.window {
                entry["callgrind"]["window"] = json!(window);
            }
        }
        if let (Some(a), Some(b)) = (m.allocs, m.bytes) {
            entry["alloc"] = json!({ "allocs": a, "bytes": b });
        }
        if let (Some(p), Some(w)) = (m.polls, m.wakes) {
            entry["asyncexec"] = json!({ "polls": p, "wakes": w });
        }
        if let (Some(ms), Some(sz)) = (m.build_ms, m.size_bytes) {
            entry["buildcost"] = json!({ "build_ms": ms, "size_bytes": sz });
        }
        if let Some(prefix) = &m.imported {
            entry["imported"] = json!(prefix);
        }
        items.insert(id.clone(), entry);
    }
    Value::Object(items)
}

/// Deserialize the baseline `items` shape back into a `Run`. The mirror of
/// `run_to_items_value`, for reusing a cached reference-shaped doc as a live
/// measurement instead of remeasuring it.
pub fn run_from_items_value(items: &Value) -> Run {
    let mut run = Run::default();
    let Some(map) = items.as_object() else {
        return run;
    };
    for (id, v) in map {
        run.items.insert(
            id.clone(),
            ItemMetrics {
                fingerprint: v["fingerprint"].as_str().unwrap_or("").to_string(),
                covers: v["covers"].as_str().unwrap_or("").to_string(),
                tolerance_pct: v["tolerance_pct"].as_f64(),
                median_ns: v["walltime"]["median_ns"].as_f64(),
                mad_ns: v["walltime"]["mad_ns"].as_f64(),
                p99_ns: v["walltime"]["p99_ns"].as_f64(),
                wall_rounds: v["walltime"]["rounds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_f64)
                    .collect(),
                instructions: v["perfcnt"]["instructions"].as_u64(),
                cycles: v["perfcnt"]["cycles"].as_u64(),
                cache_refs: v["perfcnt"]["cache_refs"].as_u64(),
                ir: v["callgrind"]["ir"].as_u64(),
                window: v["callgrind"]["window"].as_u64(),
                allocs: v["alloc"]["allocs"].as_u64(),
                bytes: v["alloc"]["bytes"].as_u64(),
                polls: v["asyncexec"]["polls"].as_u64(),
                wakes: v["asyncexec"]["wakes"].as_u64(),
                build_ms: v["buildcost"]["build_ms"].as_u64(),
                size_bytes: v["buildcost"]["size_bytes"].as_u64(),
                reused: None,
                imported: v["imported"].as_str().map(String::from),
            },
        );
    }
    run
}

/// `run`'s items as a baseline stores them, each marked with what it reused.
/// Per item, since runs of several packages share one baseline file.
pub(super) fn saved_items(run: &Run) -> Value {
    let mut items = run_to_items_value(run);
    if let Some(map) = items.as_object_mut() {
        for (id, item) in map {
            if let Some(reused) = run.items[id].reused.as_ref().or(run.reused.as_ref()) {
                item["reused"] = json!({ "from": reused.from, "metrics": reused.metrics });
            }
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::{ItemMetrics, Reused, Run, run_from_items_value, run_to_items_value, saved_items};

    #[test]
    fn a_saved_item_says_which_metrics_it_reused() {
        let mut run = Run::default();
        run.items.insert("pkg::a".into(), ItemMetrics::default());
        assert!(saved_items(&run)["pkg::a"].get("reused").is_none());
        run.reused = Some(Reused {
            from: "binary 0123456789abcdef".into(),
            metrics: vec!["callgrind.ir"],
        });
        let item = &saved_items(&run)["pkg::a"];
        assert_eq!(item["reused"]["from"], "binary 0123456789abcdef");
        assert_eq!(
            item["reused"]["metrics"],
            serde_json::json!(["callgrind.ir"])
        );
    }

    #[test]
    fn executed_records_are_kept_only_under_callgrind() {
        let executed = serde_json::json!({
            "type": "executed", "id": "pkg::a", "functions": [["/bin/bench", "f"]],
        });
        let under = |backend: &str| {
            super::collect(&[
                serde_json::json!({ "type": "env", "gating_backend": backend }),
                executed.clone(),
            ])
        };
        assert_eq!(
            under("callgrind").executed["pkg::a"],
            [("/bin/bench".to_string(), "f".to_string())]
        );
        assert!(under("perfcnt").executed.is_empty());
    }

    #[test]
    fn a_saved_item_carries_its_own_reuse_marker() {
        let mut run = Run::default();
        run.items.insert("pkg::a".into(), ItemMetrics::default());
        run.items.insert(
            "pkg::b".into(),
            ItemMetrics {
                reused: Some(Reused {
                    from: "item cache".into(),
                    metrics: vec!["callgrind.ir"],
                }),
                ..ItemMetrics::default()
            },
        );
        let items = saved_items(&run);
        assert!(items["pkg::a"].get("reused").is_none());
        assert_eq!(items["pkg::b"]["reused"]["from"], "item cache");
    }

    #[test]
    fn run_from_items_value_round_trips_run_to_items_value() {
        let mut run = Run::default();
        run.items.insert(
            "pkg::bench".into(),
            ItemMetrics {
                fingerprint: "fp".into(),
                covers: "pkg::covered".into(),
                median_ns: Some(100.0),
                mad_ns: Some(1.0),
                p99_ns: Some(120.0),
                wall_rounds: vec![95.0, 105.0],
                instructions: Some(500),
                cycles: Some(600),
                cache_refs: Some(10),
                ir: Some(700),
                window: Some(10),
                allocs: Some(2),
                bytes: Some(64),
                polls: Some(3),
                wakes: Some(3),
                tolerance_pct: Some(8.0),
                build_ms: None,
                size_bytes: None,
                reused: None,
                imported: Some("jmh".into()),
            },
        );
        let round_tripped = run_from_items_value(&run_to_items_value(&run));
        let original = &run.items["pkg::bench"];
        let back = &round_tripped.items["pkg::bench"];
        assert_eq!(back.fingerprint, original.fingerprint);
        assert_eq!(back.covers, original.covers);
        assert_eq!(back.median_ns, original.median_ns);
        assert_eq!(back.mad_ns, original.mad_ns);
        assert_eq!(back.p99_ns, original.p99_ns);
        assert_eq!(back.imported, original.imported);
        assert_eq!(back.wall_rounds, original.wall_rounds);
        assert_eq!(back.instructions, original.instructions);
        assert_eq!(back.cycles, original.cycles);
        assert_eq!(back.cache_refs, original.cache_refs);
        assert_eq!(back.ir, original.ir);
        assert_eq!(back.allocs, original.allocs);
        assert_eq!(back.bytes, original.bytes);
        assert_eq!(back.polls, original.polls);
        assert_eq!(back.wakes, original.wakes);
        assert_eq!(back.tolerance_pct, original.tolerance_pct);
    }
}
