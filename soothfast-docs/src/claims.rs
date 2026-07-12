//! Quantitative-claims ledger: `item.backend.metric <op> value[unit]`
//! evaluated against a measurement baseline. Numbers in prose become gated
//! facts instead of aspirations.

use serde_json::Value;

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

/// One parsed `soothfast:claim` expression: a measured metric of an item,
/// compared against a fixed bound.
#[derive(Debug, Clone)]
pub struct Claim {
    /// Fully qualified item path (`demo::lcg_checksum`).
    pub item: String,
    /// Metric backend the value comes from (`perfcnt`, `walltime`, `alloc`).
    pub backend: String,
    /// Metric name within the backend (`instructions`, `p99`, `allocs`).
    pub metric: String,
    /// How the measured value is compared to `bound`.
    pub op: Op,
    /// Claimed limit, in the metric's raw unit (durations normalized to ns).
    pub bound: f64,
}

/// Parse `demo::lcg_checksum.perfcnt.instructions < 25000` (units on the
/// bound: ns/us/ms/s multiply into ns; bare numbers stay raw).
pub fn parse(expr: &str) -> Result<Claim, String> {
    let tokens: Vec<&str> = expr.split_whitespace().collect();
    let [lhs, op, rhs] = tokens.as_slice() else {
        return Err(format!(
            "claim must be `item.backend.metric <op> value`, got {expr:?}"
        ));
    };
    let op = match *op {
        "<" => Op::Lt,
        "<=" => Op::Le,
        ">" => Op::Gt,
        ">=" => Op::Ge,
        other => return Err(format!("unsupported operator {other:?}")),
    };

    // Item ids contain `::` but never `.` — split lhs on dots.
    let parts: Vec<&str> = lhs.split('.').collect();
    if parts.len() != 3 {
        return Err(format!(
            "left side must be item.backend.metric, got {lhs:?}"
        ));
    }

    let unit_at = rhs
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '_')
        .unwrap_or(rhs.len());
    let (num, unit) = rhs.split_at(unit_at);
    let value: f64 = num
        .replace('_', "")
        .parse()
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
        item: parts[0].to_string(),
        backend: parts[1].to_string(),
        metric: parts[2].to_string(),
        op,
        bound: value * factor,
    })
}

/// Evaluate against a baseline document; returns (holds, actual value).
pub fn evaluate(claim: &Claim, baseline: &Value) -> Result<(bool, f64), String> {
    let actual = baseline["items"][&claim.item][&claim.backend][&claim.metric]
        .as_f64()
        .ok_or_else(|| {
            format!(
                "no measurement for {}.{}.{} in baseline — run `cargo soothfast measure --save-baseline`",
                claim.item, claim.backend, claim.metric
            )
        })?;
    let holds = match claim.op {
        Op::Lt => actual < claim.bound,
        Op::Le => actual <= claim.bound,
        Op::Gt => actual > claim.bound,
        Op::Ge => actual >= claim.bound,
    };
    Ok((holds, actual))
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
    fn rejects_malformed() {
        assert!(parse("demo::f.walltime < 5").is_err());
        assert!(parse("demo::f.walltime.median == 5").is_err());
        assert!(parse("demo::f.walltime.median < 5parsecs").is_err());
    }
}
