//! Per-probe sanity assertions: `meta.regularMarketPrice > 0`.
//!
//! Population says a field exists; an assertion says its value is sane.
//! The language is one comparison per line — `path op literal` — because
//! anything richer belongs in a real test, not a probes manifest:
//!
//! ```text
//! meta.regularMarketPrice > 0
//! candles#len >= 1
//! candles[].close > 0
//! meta.currency == "USD"
//! symbol exists
//! ```
//!
//! `[]` descends into every array element and the assertion must hold for
//! each; `#len` compares an array or string length. Missing or null
//! values fail comparisons (an absent price is not `> 0`) but `!=`
//! against a literal still holds.

use serde_json::Value;

/// One parsed assertion, kept with its source line for reporting.
#[derive(Debug, Clone)]
pub struct Assertion {
    source: String,
    path: Vec<Step>,
    length: bool,
    op: Op,
    literal: Value,
}

#[derive(Debug, Clone, PartialEq)]
enum Step {
    Key(String),
    Elements,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Exists,
}

impl Assertion {
    pub fn parse(line: &str) -> Result<Assertion, String> {
        let mut parts = line.split_whitespace();
        let path_text = parts.next().ok_or("empty assertion")?;
        let op_text = parts
            .next()
            .ok_or_else(|| format!("`{line}`: missing operator"))?;
        let op = match op_text {
            "==" => Op::Eq,
            "!=" => Op::Ne,
            ">" => Op::Gt,
            ">=" => Op::Ge,
            "<" => Op::Lt,
            "<=" => Op::Le,
            "exists" => Op::Exists,
            other => return Err(format!("`{line}`: unknown operator `{other}`")),
        };
        let literal = if op == Op::Exists {
            if parts.next().is_some() {
                return Err(format!("`{line}`: `exists` takes no operand"));
            }
            Value::Null
        } else {
            let rest = parts.collect::<Vec<_>>().join(" ");
            serde_json::from_str(&rest)
                .map_err(|_| format!("`{line}`: operand `{rest}` is not a JSON literal"))?
        };
        let (path_text, length) = match path_text.strip_suffix("#len") {
            Some(stripped) => (stripped, true),
            None => (path_text, false),
        };
        let mut path = Vec::new();
        for segment in path_text.split('.').filter(|s| !s.is_empty()) {
            match segment.strip_suffix("[]") {
                Some("") => path.push(Step::Elements),
                Some(key) => {
                    path.push(Step::Key(key.to_string()));
                    path.push(Step::Elements);
                }
                None => path.push(Step::Key(segment.to_string())),
            }
        }
        Ok(Assertion {
            source: line.to_string(),
            path,
            length,
            op,
            literal,
        })
    }

    /// The assertion's source line, for failure messages.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Check the assertion against a response. `Err` carries the reason.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        let mut targets = vec![value];
        for step in &self.path {
            let mut next = Vec::new();
            for target in targets {
                match step {
                    Step::Key(key) => next.push(target.get(key).unwrap_or(&Value::Null)),
                    Step::Elements => match target.as_array() {
                        Some(items) => next.extend(items.iter()),
                        None => next.push(&Value::Null),
                    },
                }
            }
            targets = next;
        }
        if targets.is_empty() {
            // An empty array under `[]` leaves nothing to falsify.
            return Ok(());
        }
        for target in targets {
            self.check_one(target)?;
        }
        Ok(())
    }

    fn check_one(&self, target: &Value) -> Result<(), String> {
        let length_value;
        let target = if self.length {
            let len = match target {
                Value::Array(items) => items.len(),
                Value::String(s) => s.len(),
                other => return Err(format!("{}: {} has no length", self.source, kind(other))),
            };
            length_value = Value::from(len);
            &length_value
        } else {
            target
        };
        let holds = match self.op {
            Op::Exists => !target.is_null(),
            Op::Eq => target == &self.literal,
            Op::Ne => target != &self.literal,
            Op::Gt | Op::Ge | Op::Lt | Op::Le => {
                let (Some(actual), Some(expected)) = (target.as_f64(), self.literal.as_f64())
                else {
                    return Err(format!("{}: got {}", self.source, short(target)));
                };
                match self.op {
                    Op::Gt => actual > expected,
                    Op::Ge => actual >= expected,
                    Op::Lt => actual < expected,
                    _ => actual <= expected,
                }
            }
        };
        if holds {
            Ok(())
        } else {
            Err(format!("{}: got {}", self.source, short(target)))
        }
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn short(value: &Value) -> String {
    let text = value.to_string();
    if text.len() > 60 {
        format!("{}…", &text[..60])
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assert_holds(line: &str, value: &Value) {
        Assertion::parse(line).unwrap().check(value).unwrap();
    }

    fn assert_fails(line: &str, value: &Value) {
        assert!(Assertion::parse(line).unwrap().check(value).is_err());
    }

    #[test]
    fn scalar_comparisons() {
        let value = json!({"meta": {"price": 12.5, "currency": "USD"}});
        assert_holds("meta.price > 0", &value);
        assert_holds("meta.currency == \"USD\"", &value);
        assert_fails("meta.price < 1", &value);
        assert_fails("meta.currency == \"EUR\"", &value);
    }

    #[test]
    fn missing_and_null_fail_comparisons_but_exist_checks_say_why() {
        let value = json!({"meta": {"price": null}});
        assert_fails("meta.price > 0", &value);
        assert_fails("meta.gone > 0", &value);
        assert_fails("meta.price exists", &value);
        assert_holds("meta.price != 3", &value);
    }

    #[test]
    fn length_of_arrays_and_strings() {
        let value = json!({"candles": [1, 2, 3], "symbol": "AAPL"});
        assert_holds("candles#len >= 1", &value);
        assert_holds("symbol#len == 4", &value);
        assert_fails("candles#len > 3", &value);
        assert_fails("candles.0#len > 0", &value);
    }

    #[test]
    fn element_descent_must_hold_for_every_element() {
        let value = json!({"candles": [{"close": 1.0}, {"close": 0.0}]});
        assert_fails("candles[].close > 0", &value);
        assert_holds("candles[].close >= 0", &value);
        assert_holds("empty[].close > 0", &json!({"empty": []}));
    }

    #[test]
    fn parse_rejects_malformed_lines() {
        assert!(Assertion::parse("price >").is_err());
        assert!(Assertion::parse("price ~ 3").is_err());
        assert!(Assertion::parse("price exists 3").is_err());
        assert!(Assertion::parse("price == not-json").is_err());
    }
}
