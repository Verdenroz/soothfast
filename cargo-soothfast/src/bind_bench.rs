//! `bind bench` protocol: one JSON object per line on a host script's
//! stdout, `{"shape": "...", "binding_ns": <f64>, "host_ns": <f64>, "n":
//! <u64>}`. Anything else on the line is ignored, since stderr, not stdout,
//! is where a script's own diagnostics belong.

use serde_json::Value;
use std::collections::BTreeSet;

/// One shape's best-of-K timings, as a script reported them.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchRecord {
    pub shape: String,
    pub binding_ns: f64,
    pub host_ns: f64,
    pub n: u64,
}

/// Parse one line, or `None` if it is not a well-formed record: malformed
/// JSON, missing fields, or a line that isn't JSON at all.
fn parse_line(line: &str) -> Option<BenchRecord> {
    let v: Value = serde_json::from_str(line).ok()?;
    Some(BenchRecord {
        shape: v["shape"].as_str()?.to_string(),
        binding_ns: v["binding_ns"].as_f64()?,
        host_ns: v["host_ns"].as_f64()?,
        n: v["n"].as_u64()?,
    })
}

/// A shape name has to survive `item.backend.metric` claim parsing (which
/// splits on `.`) and read cleanly in a baseline id, so it's held to the
/// same identifier shape as a Rust item name.
fn valid_shape(shape: &str) -> bool {
    let mut chars = shape.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Reject a record a script printed correctly-shaped but with nonsense
/// values: a zero or negative timing divides into an infinite or negative
/// ratio, and `n` of zero means nothing was measured.
fn validate(rec: &BenchRecord) -> Result<(), String> {
    if !valid_shape(&rec.shape) {
        return Err(format!("shape {:?} is not a valid identifier", rec.shape));
    }
    if !rec.binding_ns.is_finite() || rec.binding_ns <= 0.0 {
        return Err(format!(
            "shape {:?}: binding_ns must be a positive finite number, got {}",
            rec.shape, rec.binding_ns
        ));
    }
    if !rec.host_ns.is_finite() || rec.host_ns <= 0.0 {
        return Err(format!(
            "shape {:?}: host_ns must be a positive finite number, got {}",
            rec.shape, rec.host_ns
        ));
    }
    if rec.n == 0 {
        return Err(format!("shape {:?}: n must be nonzero", rec.shape));
    }
    Ok(())
}

/// Parse a script's full stdout into its reported shapes.
///
/// Errors if a record's values are nonsense, the same shape is printed
/// twice (ambiguous which reading is the real one), or nothing valid was
/// printed at all (a script that ran but measured nothing is a setup bug,
/// not an empty result).
pub fn parse_output(stdout: &str) -> Result<Vec<BenchRecord>, String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Some(rec) = parse_line(line) else {
            continue;
        };
        validate(&rec)?;
        if !seen.insert(rec.shape.clone()) {
            return Err(format!("shape {:?} printed twice", rec.shape));
        }
        out.push(rec);
    }
    if out.is_empty() {
        return Err("script printed no shapes".to_string());
    }
    Ok(out)
}

/// How much the binding wins by. Above 1.0 means the binding beats the host
/// language; below 1.0 means the crossing cost more than the work.
pub fn ratio(binding_ns: f64, host_ns: f64) -> f64 {
    host_ns / binding_ns
}

/// The baseline item id one shape's ratio is filed under:
/// `<crate>::bind::<lang>::<shape>`. Matches the normalized-crate-name form
/// every other measured item id already uses, so `soothfast:claim
/// <id>.ratio.ratio` resolves the same way `alloc`/`walltime` claims do.
pub fn item_id(pkg: &str, lang: &str, shape: &str) -> String {
    format!("{}::bind::{lang}::{shape}", pkg.replace('-', "_"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_valid_line() {
        let line =
            r#"{"shape": "build_summary", "binding_ns": 120.5, "host_ns": 760.0, "n": 100000}"#;
        let recs = parse_output(line).expect("one valid shape");
        assert_eq!(
            recs,
            vec![BenchRecord {
                shape: "build_summary".into(),
                binding_ns: 120.5,
                host_ns: 760.0,
                n: 100000,
            }]
        );
    }

    #[test]
    fn ignores_junk_lines() {
        let stdout = "starting up...\n\
                       {\"shape\": \"batch_buffer\", \"binding_ns\": 10.0, \"host_ns\": 41.0, \"n\": 100000}\n\
                       {not json}\n\
                       {\"shape\": \"per_element\"}\n\
                       done\n";
        let recs = parse_output(stdout).expect("one valid shape among junk");
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].shape, "batch_buffer");
    }

    #[test]
    fn rejects_a_shape_printed_twice() {
        let stdout = "{\"shape\": \"build_summary\", \"binding_ns\": 1.0, \"host_ns\": 2.0, \"n\": 1}\n\
                       {\"shape\": \"build_summary\", \"binding_ns\": 3.0, \"host_ns\": 4.0, \"n\": 1}\n";
        let err = parse_output(stdout).expect_err("duplicate shape");
        assert!(err.contains("build_summary"), "{err}");
    }

    #[test]
    fn rejects_empty_output() {
        let err = parse_output("no shapes here, just noise\n").expect_err("no shapes");
        assert!(err.contains("no shapes"), "{err}");
    }

    #[test]
    fn rejects_a_shape_that_is_not_an_identifier() {
        let line = r#"{"shape": "batch buffer", "binding_ns": 1.0, "host_ns": 2.0, "n": 1}"#;
        let err = parse_output(line).expect_err("bad shape");
        assert!(err.contains("batch buffer"), "{err}");
    }

    #[test]
    fn rejects_a_nonpositive_binding_ns() {
        let line = r#"{"shape": "build_summary", "binding_ns": 0, "host_ns": 2.0, "n": 1}"#;
        let err = parse_output(line).expect_err("zero binding_ns");
        assert!(err.contains("binding_ns"), "{err}");
    }

    // JSON itself can't carry NaN/Infinity: serde_json rejects an
    // out-of-range literal as a parse error before a value ever exists to
    // validate, so this exercises `validate` directly against a value a
    // script could only produce by a bug in how it's computed, not printed.
    #[test]
    fn rejects_a_nonfinite_host_ns() {
        let rec = BenchRecord {
            shape: "build_summary".into(),
            binding_ns: 1.0,
            host_ns: f64::INFINITY,
            n: 1,
        };
        let err = validate(&rec).expect_err("infinite host_ns");
        assert!(err.contains("host_ns"), "{err}");
    }

    #[test]
    fn rejects_a_zero_n() {
        let line = r#"{"shape": "build_summary", "binding_ns": 1.0, "host_ns": 2.0, "n": 0}"#;
        let err = parse_output(line).expect_err("zero n");
        assert!(err.contains("nonzero"), "{err}");
    }

    #[test]
    fn ratio_above_one_means_the_binding_wins() {
        assert_eq!(ratio(10.0, 40.0), 4.0);
        assert_eq!(ratio(40.0, 10.0), 0.25);
    }

    #[test]
    fn item_id_normalizes_the_crate_name() {
        assert_eq!(
            item_id("soothfast-demo", "node", "batch_buffer"),
            "soothfast_demo::bind::node::batch_buffer"
        );
    }
}
