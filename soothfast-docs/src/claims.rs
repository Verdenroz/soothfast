//! Quantitative-claims ledger: `item.backend.metric <op> value[unit]`
//! evaluated against a measurement baseline. Numbers in prose become gated
//! facts instead of aspirations.

use serde_json::Value;

use crate::baseline::Number;

/// Comparison operator of a claim expression (`<`, `<=`, `>`, `>=`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    /// `<`
    Lt,
    /// `<=` (also written `≤`)
    Le,
    /// `>`
    Gt,
    /// `>=` (also written `≥`)
    Ge,
}

/// A metric of one item: `item.backend.metric`.
#[derive(Debug, Clone)]
pub struct Operand {
    /// Fully qualified item path (`demo::lcg_checksum`).
    pub item: String,
    /// Metric backend the value comes from (`perfcnt`, `walltime`, `alloc`).
    pub backend: String,
    /// Metric name within the backend (`instructions`, `p99`, `allocs`).
    pub metric: String,
}

/// One parsed `soothfast:claim` expression: a measured metric of an item,
/// or the difference of two items' metrics, compared against a fixed bound.
#[derive(Debug, Clone)]
pub struct Claim {
    /// Fully qualified item path (`demo::lcg_checksum`).
    pub item: String,
    /// Metric backend the value comes from (`perfcnt`, `walltime`, `alloc`).
    pub backend: String,
    /// Metric name within the backend (`instructions`, `p99`, `allocs`).
    pub metric: String,
    /// Metric subtracted from the one above (`A.b.m - B.b.m < 2us`).
    pub minus: Option<Operand>,
    /// How the measured value is compared to `bound`.
    pub op: Op,
    /// Claimed limit, in the metric's raw unit (durations normalized to ns).
    pub bound: f64,
}

/// Parse `demo::lcg_checksum.perfcnt.instructions < 25000` (units on the
/// bound: ns/us/ms/s multiply into ns; bare numbers stay raw). Two operands
/// joined by ` - ` claim their difference.
pub fn parse(expr: &str) -> Result<Claim, String> {
    // Imported item ids may hold spaces (`file > group > name`), so the
    // operator and bound are the last two tokens and the rest is the items.
    let malformed = || format!("claim must be `item.backend.metric <op> value`, got {expr:?}");
    let (rest, rhs) = expr
        .trim()
        .rsplit_once(char::is_whitespace)
        .ok_or_else(malformed)?;
    let (lhs, op) = rest
        .trim_end()
        .rsplit_once(char::is_whitespace)
        .ok_or_else(malformed)?;
    let lhs = lhs.trim_end();
    let op = match op {
        "<" => Op::Lt,
        "<=" => Op::Le,
        ">" => Op::Gt,
        ">=" => Op::Ge,
        other => return Err(format!("unsupported operator {other:?}")),
    };

    if lhs.ends_with(" -") {
        return Err(format!(
            "claim difference needs a right operand: `A.backend.metric - B.backend.metric`, got {expr:?}"
        ));
    }
    let (first, second) = split_difference(lhs).map_or((lhs, None), |(a, b)| (a, Some(b)));
    let (item, backend, metric) = split_operand(first)?;
    let minus = second.and_then(|text| {
        let (item, backend, metric) = split_operand(text).ok()?;
        Some(Operand {
            item: item.to_string(),
            backend: backend.to_string(),
            metric: metric.to_string(),
        })
    });

    let unit_at = rhs
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '_')
        .unwrap_or(rhs.len());
    let (num, unit) = rhs.split_at(unit_at);
    let value: f64 = if num.contains('_') {
        num.replace('_', "").parse()
    } else {
        num.parse()
    }
    .map_err(|_| format!("invalid number {num:?}"))?;
    let factor = match unit {
        "" => 1.0,
        "ns" => 1.0,
        "us" | "µs" => 1e3,
        "ms" => 1e6,
        "s" => 1e9,
        other => return Err(format!("unknown unit {other:?}")),
    };

    Ok(Claim {
        item: item.to_string(),
        backend: backend.to_string(),
        metric: metric.to_string(),
        minus,
        op,
        bound: value * factor,
    })
}

/// Backends a measurement baseline records under an item.
const BACKENDS: [&str; 6] = [
    "perfcnt",
    "walltime",
    "alloc",
    "asyncexec",
    "callgrind",
    "buildcost",
];

/// The ` - ` that separates two operands, trying split points from the
/// right. An imported id may hold ` - ` itself, so a split counts only when
/// both sides are operands with a known backend.
fn split_difference(lhs: &str) -> Option<(&str, &str)> {
    let known =
        |text: &str| split_operand(text).is_ok_and(|(_, backend, _)| BACKENDS.contains(&backend));
    lhs.rmatch_indices(" - ").find_map(|(at, sep)| {
        let (a, b) = (lhs[..at].trim_end(), lhs[at + sep.len()..].trim_start());
        (known(a) && known(b)).then_some((a, b))
    })
}

/// Backend and metric never hold a dot; an imported item id may.
fn split_operand(text: &str) -> Result<(&str, &str, &str), String> {
    text.rsplit_once('.')
        .and_then(|(rest, metric)| Some((rest.rsplit_once('.')?, metric)))
        .map(|((item, backend), metric)| (item.trim_start(), backend, metric))
        .filter(|(_, backend, metric)| {
            !backend.contains(char::is_whitespace) && !metric.contains(char::is_whitespace)
        })
        .ok_or_else(|| format!("left side must be item.backend.metric, got {text:?}"))
}

/// Why a claim's numbers could not be read.
#[derive(Debug, Clone, PartialEq)]
pub enum Unread {
    /// The claim reads an item two packages both own, so no number is its.
    Ambiguous { item: String, packages: Vec<String> },
    /// Any other reason, said in full.
    Other(String),
}

impl std::fmt::Display for Unread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unread::Ambiguous { item, packages } => write!(
                f,
                "item id {item} is measured by both {}; a claim on it is ambiguous",
                packages.join(" and ")
            ),
            Unread::Other(why) => f.write_str(why),
        }
    }
}

/// The measured value of the claim's item and, for a difference, of the item
/// subtracted from it.
/// An operand two packages own fails the claim as ambiguous, whichever side
/// it is on and whatever the other side reads.
pub fn values(claim: &Claim, baseline: &Value) -> Result<(f64, Option<f64>), Unread> {
    let items = std::iter::once(&claim.item).chain(claim.minus.iter().map(|m| &m.item));
    for item in items {
        let packages = crate::baseline::ambiguous_packages(baseline, item);
        if !packages.is_empty() {
            let item = item.clone();
            return Err(Unread::Ambiguous { item, packages });
        }
    }
    let first = measured(baseline, &claim.item, &claim.backend, &claim.metric)?;
    let second = match &claim.minus {
        Some(m) => Some(measured(baseline, &m.item, &m.backend, &m.metric)?),
        None => None,
    };
    Ok((first, second))
}

/// Evaluate against a baseline document; returns (holds, actual value).
pub fn evaluate(claim: &Claim, baseline: &Value) -> Result<(bool, f64), Unread> {
    let (first, second) = values(claim, baseline).map_err(|e| match e {
        Unread::Other(e) if claim.minus.is_none() && claim.item.contains(" - ") => {
            Unread::Other(format!(
                "{e} (read as one item; for a difference both sides need a known backend: {})",
                BACKENDS.join(", ")
            ))
        }
        e => e,
    })?;
    let actual = first - second.unwrap_or(0.0);
    let holds = match claim.op {
        Op::Lt => actual < claim.bound,
        Op::Le => actual <= claim.bound,
        Op::Gt => actual > claim.bound,
        Op::Ge => actual >= claim.bound,
    };
    Ok((holds, actual))
}

fn measured(baseline: &Value, item: &str, backend: &str, metric: &str) -> Result<f64, Unread> {
    match crate::baseline::number(baseline, item, backend, metric) {
        Number::Measured(v) => Ok(v),
        Number::Ambiguous(packages) => Err(Unread::Ambiguous {
            item: item.to_string(),
            packages,
        }),
        Number::Missing => Err(Unread::Other(format!(
            "no measurement for {item}.{backend}.{metric} in baseline — run `cargo soothfast measure --save-baseline`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_units_and_ops() {
        let c = parse("demo::f.walltime.median_ns < 1ms").unwrap();
        assert_eq!(c.item, "demo::f");
        assert_eq!(c.backend, "walltime");
        assert_eq!(c.metric, "median_ns");
        assert_eq!(c.bound, 1e6);
        assert!(matches!(c.op, Op::Lt));
        assert_eq!(parse("a::b.alloc.allocs <= 2").unwrap().bound, 2.0);
    }

    #[test]
    fn imported_ids_may_hold_dots_and_spaces() {
        let c = parse("pytest::t.py::test_parse.walltime.median_ns < 1us").unwrap();
        assert_eq!(c.item, "pytest::t.py::test_parse");
        assert_eq!(
            (c.backend.as_str(), c.metric.as_str()),
            ("walltime", "median_ns")
        );
        let v = parse("vitest::src/a.bench.ts > sum > loop.walltime.p99_ns <= 5ms").unwrap();
        assert_eq!(v.item, "vitest::src/a.bench.ts > sum > loop");
        assert_eq!(v.metric, "p99_ns");
        assert!(parse("walltime.median_ns < 1").is_err());
        assert!(parse("< 1").is_err());
    }

    #[test]
    fn evaluates_against_baseline() {
        let baseline = json!({ "items": { "demo::f": { "perfcnt": { "instructions": 100.0 } } } });
        let c = parse("demo::f.perfcnt.instructions < 200").unwrap();
        assert_eq!(evaluate(&c, &baseline).unwrap(), (true, 100.0));
        let c = parse("demo::f.perfcnt.instructions < 50").unwrap();
        assert_eq!(evaluate(&c, &baseline).unwrap(), (false, 100.0));
        let missing = parse("demo::g.perfcnt.instructions < 50").unwrap();
        assert!(evaluate(&missing, &baseline).is_err());
    }

    #[test]
    fn a_difference_of_two_items_parses_both_sides() {
        let c = parse("a::f.walltime.median_ns - b::g.walltime.median_ns < 2us").unwrap();
        assert_eq!(c.item, "a::f");
        let m = c.minus.unwrap();
        assert_eq!(
            (m.item.as_str(), m.backend.as_str(), m.metric.as_str()),
            ("b::g", "walltime", "median_ns")
        );
        assert_eq!(c.bound, 2e3);
        let v =
            parse("x.bench.ts > a > b.walltime.p99_ns - y.py::t.walltime.p99_ns <= 1ms").unwrap();
        assert_eq!(v.item, "x.bench.ts > a > b");
        assert_eq!(v.minus.unwrap().item, "y.py::t");
        assert!(parse("a.walltime.median_ns - b.walltime < 2us").is_err());
        let err = parse("a.walltime.median_ns - < 2us").unwrap_err();
        assert!(err.contains("right operand"), "{err}");
    }

    #[test]
    fn an_imported_id_with_a_dash_is_one_operand_or_one_side() {
        let single = parse("vitest::f.bench.ts > d - n.walltime.p99_ns < 5ms").unwrap();
        assert_eq!(single.item, "vitest::f.bench.ts > d - n");
        assert!(single.minus.is_none());
        let diff =
            parse("x.walltime.p99_ns - vitest::f.bench.ts > d - n.walltime.p99_ns < 5ms").unwrap();
        assert_eq!(diff.item, "x");
        assert_eq!(diff.minus.unwrap().item, "vitest::f.bench.ts > d - n");
    }

    #[test]
    fn a_dash_in_a_single_operand_hints_at_the_difference_form() {
        let c = parse("a.walltime.median_ns - b.nope.median_ns < 2us").unwrap();
        let err = evaluate(&c, &json!({})).unwrap_err().to_string();
        assert!(err.contains("read as one item"), "{err}");
        assert!(err.contains("perfcnt, walltime"), "{err}");
    }

    #[test]
    fn a_difference_is_evaluated_and_names_the_missing_side() {
        let baseline = json!({ "items": {
            "a": { "walltime": { "median_ns": 5000.0 } },
            "b": { "walltime": { "median_ns": 4000.0 } },
        } });
        let c = parse("a.walltime.median_ns - b.walltime.median_ns < 2us").unwrap();
        assert_eq!(evaluate(&c, &baseline).unwrap(), (true, 1000.0));
        let c = parse("a.walltime.median_ns - b.walltime.median_ns > 2us").unwrap();
        assert_eq!(evaluate(&c, &baseline).unwrap(), (false, 1000.0));
        let c = parse("a.walltime.median_ns - gone.walltime.median_ns < 2us").unwrap();
        assert!(
            evaluate(&c, &baseline)
                .unwrap_err()
                .to_string()
                .contains("gone.walltime.median_ns")
        );
        let c = parse("gone.walltime.median_ns - b.walltime.median_ns < 2us").unwrap();
        assert!(
            evaluate(&c, &baseline)
                .unwrap_err()
                .to_string()
                .contains("gone.walltime.median_ns")
        );
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse("demo::f.walltime < 5").is_err());
        assert!(parse("demo::f.walltime.median == 5").is_err());
        assert!(parse("demo::f.walltime.median < 5parsecs").is_err());
    }
}
