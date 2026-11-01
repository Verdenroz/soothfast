//! `cargo soothfast import`: turn another harness's benchmark results into a
//! soothfast run, so baselines and `gate` work for projects whose
//! measurement stays native to their language.
//!
//! Only walltime is imported. Item ids are `<prefix>::<name>`, where the
//! prefix defaults to the format and the name is whatever that harness
//! calls the benchmark, so an id is stable as long as the benchmark is.

pub mod failed;
mod jmh;
mod pyperf;
mod pytest;
#[cfg(test)]
mod tests;
mod vitest;

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::invoke::{self, ItemMetrics, Run};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    Jmh,
    Pyperf,
    Pytest,
    Vitest,
}

impl Format {
    pub fn parse(name: &str) -> Result<Format, String> {
        match name {
            "jmh" => Ok(Format::Jmh),
            "pyperf" => Ok(Format::Pyperf),
            "pytest-benchmark" => Ok(Format::Pytest),
            "vitest" => Ok(Format::Vitest),
            other => Err(format!("unknown import format {other:?}")),
        }
    }

    pub fn default_prefix(self) -> &'static str {
        match self {
            Format::Jmh => "jmh",
            Format::Pyperf => "pyperf",
            Format::Pytest => "pytest",
            Format::Vitest => "vitest",
        }
    }
}

/// One benchmark's timing summary, in nanoseconds per operation.
pub struct Timing {
    pub name: String,
    pub median_ns: f64,
    pub mad_ns: f64,
    pub p99_ns: Option<f64>,
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

/// Parse a result file into a run whose items are keyed `<prefix>::<name>`.
pub fn parse(format: Format, text: &str, prefix: Option<&str>) -> Result<Run, String> {
    let timings = match format {
        Format::Jmh => jmh::parse(text)?,
        Format::Pyperf => pyperf::parse(text)?,
        Format::Pytest => pytest::parse(text, &invoke::project_root())?,
        Format::Vitest => vitest::parse(text)?,
    };
    if timings.is_empty() {
        return Err("no benchmarks in the input".into());
    }
    let prefix = prefix.unwrap_or(format.default_prefix());
    let mut run = Run {
        gating_backend: Some("walltime".into()),
        ..Run::default()
    };
    for t in timings {
        let id = format!("{prefix}::{}", t.name);
        let metrics = ItemMetrics {
            median_ns: Some(t.median_ns),
            mad_ns: Some(t.mad_ns),
            p99_ns: t.p99_ns,
            imported: Some(prefix.to_string()),
            ..ItemMetrics::default()
        };
        if run.items.insert(id.clone(), metrics).is_some() {
            return Err(format!("duplicate benchmark id {id:?}"));
        }
    }
    Ok(run)
}

/// Read and parse a result file named by its `--from` format, returning the
/// run with the id prefix it used. Callers check the prefix once with
/// [`check_prefixes`].
pub fn read(format: &str, file: &str, prefix: Option<&str>) -> Result<(Run, String), String> {
    let format = Format::parse(format)?;
    let prefix = prefix.unwrap_or(format.default_prefix()).to_string();
    let text = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
    let run = parse(format, &text, Some(&prefix))?;
    Ok((run, prefix))
}

/// The id prefixes of every imported project `soothfast.toml` under `root`
/// configures: its own `prefix`, else its format's. Each is checked by
/// [`check_prefixes`].
pub fn configured_prefixes(root: &std::path::Path) -> Result<Vec<String>, String> {
    let mut prefixes = Vec::new();
    for project in crate::init::manifest(root)? {
        let Some(bench) = &project.bench else {
            continue;
        };
        prefixes.push(match project.prefix {
            Some(prefix) => prefix,
            None => Format::parse(&bench.from)?.default_prefix().to_string(),
        });
    }
    check_prefixes(root, &prefixes)?;
    Ok(prefixes)
}

/// Refuse an import prefix a Rust item id of the cargo workspace at `root`
/// could start with, since its ids could not be told from that crate's.
/// With no workspace there, or one whose metadata cannot be read, a note
/// says nothing was checked.
pub fn check_prefixes(root: &std::path::Path, prefixes: &[String]) -> Result<(), String> {
    if prefixes.is_empty() {
        return Ok(());
    }
    if !root.join("Cargo.toml").is_file() {
        println!(
            "import: no cargo workspace, so the id prefixes are not checked against crate names"
        );
        return Ok(());
    }
    match crate::workspace::package_names(root) {
        Ok(names) => clashing_prefix(prefixes, &names),
        Err(e) => {
            println!("import: the id prefixes are not checked against crate names: {e}");
            Ok(())
        }
    }
}

/// Refuse an import prefix equal to one of the package `names`: a Rust item
/// id always starts with its package's name, `-` written as `_`.
fn clashing_prefix(
    prefixes: &[String],
    names: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    let norm = |name: &str| name.replace('-', "_");
    for prefix in prefixes {
        let head = norm(prefix.split("::").next().unwrap_or(prefix));
        if names.iter().any(|n| norm(n) == head) {
            return Err(format!(
                "import prefix {prefix:?} is also a package name in this workspace; give the import a distinct prefix"
            ));
        }
    }
    Ok(())
}

/// The run in the baseline file shape.
pub fn to_doc(run: &Run) -> Value {
    json!({ "version": 1, "items": invoke::run_to_items_value(run), "assertions": [] })
}

pub fn run(args: &[String]) -> i32 {
    let mut format = None;
    let mut file = None;
    let mut prefix = None;
    let mut save = None;
    let mut out = None;
    let mut failed = false;
    let mut base_results = None;
    let mut prior = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let slot = match a.as_str() {
            "--failed" => {
                failed = true;
                continue;
            }
            "--from" => &mut format,
            "--prefix" => &mut prefix,
            "--save-baseline" => &mut save,
            "--out" => &mut out,
            "--base-results" => &mut base_results,
            "--prior-baseline" => &mut prior,
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
    if !failed && (base_results.is_some() || prior.is_some()) {
        return fail(
            "--base-results and --prior-baseline name a failed import's items; pass --failed",
        );
    }
    if failed {
        let Some(path) = &out else {
            return fail("import --failed writes a file; pass --out FILE");
        };
        let root = invoke::project_root();
        let fallbacks = failed::Fallbacks {
            base_results: base_results.as_deref(),
            prior: prior.as_deref().map(|name| (root.as_path(), name)),
        };
        let stub = failed::stub(&format, &file, prefix.as_deref(), &fallbacks);
        let written = stub.and_then(|doc| {
            let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
            std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))
        });
        return match written {
            Ok(()) => {
                println!("import: {path} lists the gate's items as gate-failed");
                0
            }
            Err(e) => fail(&e),
        };
    }
    let outcome = read(&format, &file, prefix.as_deref()).and_then(|(run, prefix)| {
        check_prefixes(&invoke::project_root(), std::slice::from_ref(&prefix))?;
        let written = match (&save, &out) {
            (Some(name), _) => {
                let scope = invoke::SaveScope::Imported(prefix);
                invoke::save_baseline_in(&invoke::project_root(), name, &run, scope)
                    .map_err(|e| e.to_string())?
            }
            (_, Some(path)) => {
                let text =
                    serde_json::to_string_pretty(&to_doc(&run)).map_err(|e| e.to_string())?;
                std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
                PathBuf::from(path)
            }
            _ => unreachable!("exactly one destination was checked above"),
        };
        Ok((run.items.len(), written))
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
