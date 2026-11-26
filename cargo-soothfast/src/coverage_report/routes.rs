//! The `specified` column: whether the committed spec declares, and a probe
//! exercises, each route handler. Items that are not handlers are `n/a`.

use std::collections::BTreeMap;

use soothfast_docs::surface::Surface;
use soothfast_spec::specified::Specified;

use super::Row;
use crate::coverage_spec;
use crate::invoke::CommonArgs;

/// Each handler's public path, with whether every route it serves is
/// specified.
pub(super) fn handlers(pkg: &str, common: &CommonArgs) -> Result<BTreeMap<String, bool>, String> {
    Ok(by_handler(&coverage_spec::specified_items(pkg, common)?))
}

fn by_handler(items: &[Specified]) -> BTreeMap<String, bool> {
    let mut out: BTreeMap<String, bool> = BTreeMap::new();
    for item in items {
        let all = out.entry(item.handler.clone()).or_insert(true);
        *all &= item.is_specified();
    }
    out
}

/// `rows` with `specified` set on the items of `surf` that `handlers` names,
/// under any path the surface spells them with.
pub(super) fn attach(
    rows: Vec<Row>,
    surf: &Surface,
    handlers: &BTreeMap<String, bool>,
) -> Vec<Row> {
    let specified: BTreeMap<String, bool> = surf
        .counted()
        .into_iter()
        .filter_map(|c| {
            let verdicts: Vec<bool> = c
                .aliases
                .iter()
                .filter_map(|a| handlers.get(a.as_str()).copied())
                .collect();
            (!verdicts.is_empty()).then(|| (c.path, verdicts.iter().all(|v| *v)))
        })
        .collect();
    rows.into_iter()
        .map(|row| Row {
            specified: specified.get(&row.path).copied(),
            ..row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use soothfast_docs::surface::ItemInfo;

    use super::*;

    fn route(handler: &str, in_spec: bool, probed: Option<bool>) -> Specified {
        Specified {
            spec: "api.yaml".into(),
            operation: "op".into(),
            method: "GET".into(),
            path: "/x".into(),
            handler: handler.into(),
            in_spec,
            mismatch: None,
            probed,
        }
    }

    #[test]
    fn a_handler_is_specified_only_when_every_route_it_serves_is() {
        let got = by_handler(&[
            route("d::a", true, Some(true)),
            route("d::b", true, Some(true)),
            route("d::b", true, Some(false)),
            route("d::c", false, None),
        ]);
        assert_eq!(
            got,
            BTreeMap::from([
                ("d::a".to_string(), true),
                ("d::b".to_string(), false),
                ("d::c".to_string(), false)
            ])
        );
    }

    #[test]
    fn only_a_handler_named_under_any_alias_is_judged() {
        let info = |fingerprint| ItemInfo {
            kind: "function".into(),
            fingerprint,
            signature: String::new(),
            has_docs: true,
            example: false,
            spans: Vec::new(),
        };
        let mut surf = Surface::default();
        surf.items.insert("d::list".into(), info(1));
        surf.items.insert("d::handlers::list".into(), info(1));
        surf.items.insert("d::other".into(), info(2));
        let rows = surf
            .counted()
            .into_iter()
            .map(|c| Row {
                path: c.path,
                ..Row::default()
            })
            .collect();
        let handlers = BTreeMap::from([("d::handlers::list".to_string(), true)]);
        let rows = attach(rows, &surf, &handlers);
        let got: Vec<_> = rows
            .iter()
            .map(|r| (r.path.as_str(), r.specified))
            .collect();
        assert_eq!(got, [("d::list", Some(true)), ("d::other", None)]);
    }
}
