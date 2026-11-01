//! Reading one measured number out of a baseline. An id two packages both
//! own has no number of its own: its numbers sit in the baseline's
//! `ambiguous` side map, where readers that predate it never look, and an
//! `ambiguous-id` entry names it. Every reader asks [`number`] so none of
//! them reads either package's number as the id's.

use serde_json::Value;

/// The side map of numbers an ambiguous id's packages saved: `{id: {pkg: item}}`.
pub const AMBIGUOUS_KEY: &str = "ambiguous";

/// What a baseline holds for one metric of one item.
#[derive(Debug, Clone, PartialEq)]
pub enum Number {
    Measured(f64),
    /// Two or more packages own the id; these ones.
    Ambiguous(Vec<String>),
    Missing,
}

/// `id`'s `backend.metric` in `baseline`.
pub fn number(baseline: &Value, id: &str, backend: &str, metric: &str) -> Number {
    let packages = ambiguous_packages(baseline, id);
    if !packages.is_empty() {
        return Number::Ambiguous(packages);
    }
    match baseline["items"][id][backend][metric].as_f64() {
        Some(v) => Number::Measured(v),
        None => Number::Missing,
    }
}

/// The packages that own `id` when `baseline` marks it ambiguous, by an
/// `ambiguous-id` entry or the side map; empty when it is not.
pub fn ambiguous_packages(baseline: &Value, id: &str) -> Vec<String> {
    let entries = baseline["unmeasured"].as_array().into_iter().flatten();
    let named = entries
        .filter(|e| e["reason"] == "ambiguous-id")
        .any(|e| e["items"].as_array().into_iter().flatten().any(|i| i == id));
    let side = baseline[AMBIGUOUS_KEY][id].as_object();
    if !named && side.is_none() {
        return Vec::new();
    }
    let owners = baseline["owners"].as_object().into_iter().flatten();
    let mut packages: Vec<String> = owners
        .filter(|(_, ids)| ids.as_array().into_iter().flatten().any(|i| i == id))
        .map(|(p, _)| p.clone())
        .collect();
    if packages.is_empty() {
        packages = side
            .into_iter()
            .flat_map(|m| m.keys())
            .filter(|p| !p.is_empty())
            .cloned()
            .collect();
    }
    if packages.is_empty() {
        packages.push("more than one package".into());
    }
    packages
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_ambiguous_id_has_no_number_even_when_one_is_left_in_items() {
        let baseline = json!({
            "items": { "util::hash": { "alloc": { "allocs": 3 } }, "a::x": { "alloc": { "allocs": 1 } } },
            "owners": { "a": ["util::hash", "a::x"], "b": ["util::hash"] },
            "unmeasured": [{ "package": "a", "reason": "ambiguous-id", "items": ["util::hash"] }],
        });
        assert_eq!(
            number(&baseline, "util::hash", "alloc", "allocs"),
            Number::Ambiguous(vec!["a".into(), "b".into()])
        );
        assert_eq!(
            number(&baseline, "a::x", "alloc", "allocs"),
            Number::Measured(1.0)
        );
        assert_eq!(
            number(&baseline, "a::y", "alloc", "allocs"),
            Number::Missing
        );
        let claim = crate::claims::parse("util::hash.alloc.allocs <= 9").unwrap();
        let err = crate::claims::evaluate(&claim, &baseline).unwrap_err();
        assert!(err.contains("measured by both a and b"), "{err}");
    }

    #[test]
    fn the_side_map_alone_marks_an_id_ambiguous() {
        let baseline = json!({ "items": {}, "ambiguous": { "util::hash": { "a": {}, "b": {} } } });
        assert_eq!(
            ambiguous_packages(&baseline, "util::hash"),
            ["a".to_string(), "b".to_string()]
        );
    }
}
