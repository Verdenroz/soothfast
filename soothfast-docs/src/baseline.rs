//! Reading one measured number out of a baseline. An id two packages both
//! own has no number of its own: its numbers sit in the baseline's
//! `ambiguous` side map, where readers that predate it never look. Every
//! reader asks [`number`] so none of them reads either package's number as
//! the id's.

use serde_json::Value;

/// The side map of numbers an ambiguous id's packages saved:
/// `{id: [{"package": P, "item": {..}} | {"owners": [..], "item": {..}}]}`,
/// one element per saved number, by the package that wrote it, or by the
/// packages that owned the id when nothing named the writer.
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

/// The packages that own `id` when `baseline`'s side map holds it; empty
/// when it is not ambiguous.
pub fn ambiguous_packages(baseline: &Value, id: &str) -> Vec<String> {
    let Some(saved) = baseline[AMBIGUOUS_KEY][id].as_array() else {
        return Vec::new();
    };
    let owners = baseline["owners"].as_object().into_iter().flatten();
    let mut packages: Vec<String> = owners
        .filter(|(_, ids)| ids.as_array().into_iter().flatten().any(|i| i == id))
        .map(|(p, _)| p.clone())
        .collect();
    if packages.len() < 2 {
        let named = saved.iter().flat_map(|s| {
            let owners = s["owners"].as_array().into_iter().flatten();
            std::iter::once(&s["package"]).chain(owners)
        });
        packages.extend(named.filter_map(|p| p.as_str().map(str::to_string)));
        packages.sort();
        packages.dedup();
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
            "ambiguous": { "util::hash": [{ "package": "a", "item": {} }] },
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
        assert!(matches!(err, crate::claims::Unread::Ambiguous { .. }));
        assert!(
            err.to_string().contains("measured by both a and b"),
            "{err}"
        );
    }

    #[test]
    fn the_side_map_names_the_packages_when_owners_do_not() {
        let baseline = json!({ "items": {}, "ambiguous": { "util::hash": [
            { "package": "a", "item": {} },
            { "owners": ["a", "b"], "item": {} },
        ] } });
        assert_eq!(
            ambiguous_packages(&baseline, "util::hash"),
            ["a".to_string(), "b".to_string()]
        );
    }
}
