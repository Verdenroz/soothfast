//! pytest-benchmark `--benchmark-json` output.

use std::path::Path;

use serde_json::Value;

use super::Timing;

/// Parse a pytest-benchmark result, ids named by `fullname` with the path
/// made relative to `root`, the way it reads in a checkout of any name.
///
/// When present, the raw `stats.data` seconds are the samples.
/// Without them only summary stats exist, so the median is `stats.median` and
/// the MAD is approximated as `stats.stddev * 0.6745`, which holds for
/// roughly normal timings only. There is no p99 then.
pub fn parse(text: &str, root: &Path) -> Result<Vec<Timing>, String> {
    let doc: Value =
        serde_json::from_str(text).map_err(|e| format!("not pytest-benchmark JSON: {e}"))?;
    let benchmarks = doc["benchmarks"]
        .as_array()
        .ok_or("pytest-benchmark results need a `benchmarks` array")?;
    benchmarks.iter().map(|b| timing(b, root)).collect()
}

fn timing(bench: &Value, root: &Path) -> Result<Timing, String> {
    let fullname = bench["fullname"]
        .as_str()
        .ok_or("pytest-benchmark entry without `fullname`")?;
    let name = under_root(fullname, &|path| root.join(path).is_file());
    let stats = &bench["stats"];
    if let Some(data) = stats["data"].as_array().filter(|d| !d.is_empty()) {
        let samples = data
            .iter()
            .filter_map(Value::as_f64)
            .map(|s| s * 1e9)
            .collect();
        return Timing::from_samples(name, samples);
    }
    let field = |key: &str| {
        stats[key]
            .as_f64()
            .map(|s| s * 1e9)
            .ok_or_else(|| format!("{name}: no stats.{key}"))
    };
    Ok(Timing {
        median_ns: field("median")?,
        mad_ns: field("stddev")? * 0.6745,
        p99_ns: None,
        name,
    })
}

/// `fullname` without the leading directories its file path has above
/// `root`: pytest roots node ids at the common ancestor of the tests when no
/// ini file is found, so a checkout's own directory name can lead the path.
fn under_root(fullname: &str, exists: &dyn Fn(&str) -> bool) -> String {
    let Some((path, test)) = fullname.split_once("::") else {
        return fullname.into();
    };
    let mut tail = path;
    while !exists(tail) {
        let Some((_, rest)) = tail.split_once('/') else {
            return fullname.into();
        };
        tail = rest;
    }
    format!("{tail}::{test}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_directories_above_the_root_are_dropped() {
        let exists = |p: &str| p == "plugin/hooks/bench_hooks.py";
        let id = "scry-py/plugin/hooks/bench_hooks.py::test_x[1]";
        assert_eq!(
            under_root(id, &exists),
            "plugin/hooks/bench_hooks.py::test_x[1]"
        );
        let same = "plugin/hooks/bench_hooks.py::test_x";
        assert_eq!(under_root(same, &exists), same);
    }

    #[test]
    fn a_path_that_is_not_under_the_root_is_kept() {
        let none = |_: &str| false;
        assert_eq!(under_root("a/b.py::t", &none), "a/b.py::t");
        assert_eq!(under_root("no_path", &none), "no_path");
    }
}
