//! `cargo soothfast import`: turn another harness's benchmark results into a
//! soothfast run, so baselines and `gate` work for projects whose
//! measurement stays native to their language.
//!
//! Only walltime is imported. Item ids are `<prefix>::<name>`, where the
//! prefix defaults to the format and the name is whatever that harness
//! calls the benchmark, so an id is stable as long as the benchmark is.

mod jmh;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::invoke::{self, ItemMetrics, Run};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    Jmh,
}

impl Format {
    pub fn parse(name: &str) -> Result<Format, String> {
        match name {
            "jmh" => Ok(Format::Jmh),
            other => Err(format!("unknown import format {other:?}")),
        }
    }

    fn default_prefix(self) -> &'static str {
        match self {
            Format::Jmh => "jmh",
        }
    }
}

/// One benchmark's timing summary, in nanoseconds per operation.
pub struct Timing {
    pub name: String,
    pub median_ns: f64,
    pub mad_ns: f64,
    pub p99_ns: Option<f64>,
    pub samples: usize,
}

impl Timing {
    /// Summarize raw samples; the MAD is unscaled, as `soothfast-measure` reports it.
    pub fn from_samples(name: String, mut samples: Vec<f64>) -> Result<Timing, String> {
        if samples.is_empty() {
            return Err(format!("{name}: no samples"));
        }
        samples.sort_by(f64::total_cmp);
        let median_ns = median(&samples);
        let mut deviations: Vec<f64> = samples.iter().map(|v| (v - median_ns).abs()).collect();
        deviations.sort_by(f64::total_cmp);
        let rank = (samples.len() as f64 * 0.99).ceil() as usize;
        Ok(Timing {
            name,
            median_ns,
            mad_ns: median(&deviations),
            p99_ns: Some(samples[rank - 1]),
            samples: samples.len(),
        })
    }
}

fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// An imported benchmark with its soothfast item id.
pub struct Bench {
    pub id: String,
    pub metrics: ItemMetrics,
    pub samples: usize,
}

/// Parse a result file into benchmarks keyed `<prefix>::<name>`.
pub fn parse(format: Format, text: &str, prefix: Option<&str>) -> Result<Vec<Bench>, String> {
    let timings = match format {
        Format::Jmh => jmh::parse(text)?,
    };
    if timings.is_empty() {
        return Err("no benchmarks in the input".into());
    }
    let prefix = prefix.unwrap_or(format.default_prefix());
    let mut seen = BTreeSet::new();
    let mut benches = Vec::new();
    for t in timings {
        let id = format!("{prefix}::{}", t.name);
        if !seen.insert(id.clone()) {
            return Err(format!("duplicate benchmark id {id:?}"));
        }
        benches.push(Bench {
            id,
            metrics: ItemMetrics {
                median_ns: Some(t.median_ns),
                mad_ns: Some(t.mad_ns),
                p99_ns: t.p99_ns,
                ..ItemMetrics::default()
            },
            samples: t.samples,
        });
    }
    Ok(benches)
}

/// The benchmarks as a run, ready for `gate`.
pub fn to_run(benches: &[Bench]) -> Run {
    Run {
        gating_backend: Some("walltime".into()),
        items: benches
            .iter()
            .map(|b| (b.id.clone(), b.metrics.clone()))
            .collect(),
        ..Run::default()
    }
}

/// The benchmarks in the baseline file shape. `samples` counts the raw
/// measurements; `rounds` stays unset because `gate` reads it as per-round
/// medians.
pub fn to_doc(benches: &[Bench]) -> Value {
    let mut items = invoke::run_to_items_value(&to_run(benches));
    for b in benches {
        items[&b.id]["walltime"]["samples"] = json!(b.samples);
    }
    json!({ "version": 1, "items": items, "assertions": [] })
}

/// The project root outside a cargo workspace: the git toplevel, else the
/// current directory.
pub fn project_root() -> PathBuf {
    if let Ok(root) = invoke::workspace_root() {
        return root;
    }
    Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn baseline_file(root: &Path, name: &str) -> PathBuf {
    root.join(".soothfast")
        .join("baselines")
        .join(format!("{name}.json"))
}

/// Merge `doc`'s items into baseline `name`, replacing earlier items of the
/// same prefix so a renamed benchmark does not linger as GONE.
fn save_baseline(root: &Path, name: &str, prefix: &str, doc: &Value) -> Result<PathBuf, String> {
    let path = baseline_file(root, name);
    let mut saved = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(_) => json!({ "version": 1, "items": {}, "assertions": [] }),
    };
    let own = format!("{prefix}::");
    if let (Some(items), Some(new)) = (saved["items"].as_object_mut(), doc["items"].as_object()) {
        items.retain(|id, _| !id.starts_with(&own));
        items.extend(new.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    let dir = path.parent().expect("baseline path has a parent");
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&saved).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

pub fn run(args: &[String]) -> i32 {
    let mut format = None;
    let mut file = None;
    let mut prefix = None;
    let mut save = None;
    let mut out = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let slot = match a.as_str() {
            "--from" => &mut format,
            "--prefix" => &mut prefix,
            "--save-baseline" => &mut save,
            "--out" => &mut out,
            other if !other.starts_with('-') && file.is_none() => {
                file = Some(other.to_string());
                continue;
            }
            other => return fail(&format!("unknown import arg {other:?}")),
        };
        match it.next() {
            Some(v) => *slot = Some(v.clone()),
            None => return fail(&format!("{a} needs a value")),
        }
    }
    let (Some(format), Some(file)) = (format, file) else {
        return fail("import needs --from FORMAT and a result FILE");
    };
    if save.is_some() == out.is_some() {
        return fail("import needs exactly one of --save-baseline NAME or --out FILE");
    }
    let outcome = Format::parse(&format).and_then(|format| {
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?;
        let benches = parse(format, &text, prefix.as_deref())?;
        let doc = to_doc(&benches);
        let written = match (&save, &out) {
            (Some(name), _) => {
                let prefix = prefix.as_deref().unwrap_or(format.default_prefix());
                save_baseline(&project_root(), name, prefix, &doc)?
            }
            (_, Some(path)) => {
                let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
                std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
                PathBuf::from(path)
            }
            _ => unreachable!("exactly one destination was checked above"),
        };
        Ok((benches.len(), written))
    });
    match outcome {
        Ok((n, path)) => {
            println!("imported {n} benchmark(s): {}", path.display());
            0
        }
        Err(e) => fail(&e),
    }
}

fn fail(msg: &str) -> i32 {
    eprintln!("soothfast: {msg}");
    2
}
