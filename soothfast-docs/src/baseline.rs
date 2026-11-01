//! Reading one measured number out of a baseline. An id two packages both
//! own has no number of its own: `items` holds nothing for it, and the
//! baseline's `ambiguous` index (`{id: {packages, covers}}`) names the
//! packages instead, so a reader that predates the index finds no item.
//! Every reader asks [`number`] or [`ambiguous`] so each one says why.

use serde_json::Value;

/// The key of a baseline's index of ambiguous ids.
pub const AMBIGUOUS_KEY: &str = "ambiguous";

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

/// One id of a baseline's `ambiguous` index.
#[derive(Debug, Clone, PartialEq)]
pub struct Ambiguous<'a> {
    /// The ambiguous item id.
    pub id: &'a str,
    /// The packages that own it.
    pub packages: Vec<String>,
    /// The public path it covers, when known.
    pub covers: Option<&'a str>,
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

/// The packages the `ambiguous` index names for `id`; empty when it names none.
pub fn ambiguous_packages(baseline: &Value, id: &str) -> Vec<String> {
    packages(&baseline[AMBIGUOUS_KEY][id])
}

/// Every id of `baseline`'s `ambiguous` index.
pub fn ambiguous(baseline: &Value) -> Vec<Ambiguous<'_>> {
    let index = baseline[AMBIGUOUS_KEY].as_object().into_iter().flatten();
    index
        .map(|(id, entry)| Ambiguous {
            id,
            packages: packages(entry),
            covers: entry["covers"].as_str(),
        })
        .collect()
}

fn packages(entry: &Value) -> Vec<String> {
    let packages = entry["packages"].as_array().into_iter().flatten();
    packages
        .filter_map(|p| p.as_str().map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn baseline() -> Value {
        json!({
            "items": { "a::x": { "alloc": { "allocs": 1 } } },
            "owners": { "a": ["util::hash", "a::x"], "b": ["util::hash"] },
            "ambiguous": { "util::hash": { "packages": ["a", "b"], "covers": "util::hash_bytes" } },
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

    #[test]
    fn the_index_lists_each_ambiguous_id_with_what_it_covers() {
        let baseline = baseline();
        assert_eq!(
            ambiguous(&baseline),
            [Ambiguous {
                id: "util::hash",
                packages: vec!["a".into(), "b".into()],
                covers: Some("util::hash_bytes"),
            }]
        );
    }
}
