//! Reading one measured number out of a baseline. An id two packages both
//! own (`owners` lists it under each) has no number of its own, so `items`
//! holds none for it and a reader that predates this finds nothing. Every
//! reader asks [`number`] so each one says why.

use serde_json::Value;

/// What a baseline holds for one metric of one item.
#[derive(Debug, Clone, PartialEq)]
pub enum Number {
    /// The number the item's one owner saved.
    Measured(f64),
    /// Two or more packages own the id; these ones.
    Ambiguous(Vec<String>),
    /// No number for that metric, or no such item.
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

/// The packages that own `id` when more than one does; empty otherwise.
pub fn ambiguous_packages(baseline: &Value, id: &str) -> Vec<String> {
    let owners = baseline["owners"].as_object().into_iter().flatten();
    let packages: Vec<String> = owners
        .filter(|(_, ids)| ids.as_array().into_iter().flatten().any(|i| i == id))
        .map(|(p, _)| p.clone())
        .collect();
    if packages.len() > 1 {
        packages
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn baseline() -> Value {
        json!({
            "items": { "a::x": { "alloc": { "allocs": 1 } } },
            "owners": { "a": ["util::hash", "a::x"], "b": ["util::hash"] },
        })
    }

    #[test]
    fn an_id_two_packages_own_is_ambiguous_and_one_owner_s_is_measured() {
        let baseline = baseline();
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
    }

    #[test]
    fn a_claim_fails_as_ambiguous_whichever_operand_is() {
        let baseline = baseline();
        for expr in [
            "util::hash.alloc.allocs <= 9",
            "gone.alloc.allocs - util::hash.alloc.allocs <= 9",
            "util::hash.alloc.allocs - gone.alloc.allocs <= 9",
        ] {
            let claim = crate::claims::parse(expr).unwrap();
            let err = crate::claims::evaluate(&claim, &baseline).unwrap_err();
            assert!(
                matches!(err, crate::claims::Unread::Ambiguous { .. }),
                "{expr}"
            );
            assert!(
                err.to_string().contains("measured by both a and b"),
                "{err}"
            );
        }
    }
}
