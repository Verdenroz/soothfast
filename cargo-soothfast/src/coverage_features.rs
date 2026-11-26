//! `coverage features`: public items behind cargo features that no gate run
//! compiles, per feature. Coverage is computed under the gate's feature set,
//! so an item behind any other feature is absent from every denominator.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

const COUNTED: [&str; 6] = ["function", "struct", "enum", "union", "trait", "type_alias"];

/// One public item of a rustdoc surface.
#[derive(Debug, Default, PartialEq)]
pub struct Entry {
    /// Whether the item belongs in the denominator, as opposed to a module
    /// that only passes its `cfg` on to what it holds.
    pub counted: bool,
    /// Features the item, or anything enclosing it, is `cfg`-gated on.
    pub features: BTreeSet<String>,
}

/// What lies outside the gate's feature set.
#[derive(Debug, Default, PartialEq)]
pub struct Outside {
    /// Public items in the reference surface.
    pub reference: u32,
    /// Items the gate's surface lacks.
    pub outside: u32,
    /// Outside items per feature; an item gated on several counts under each.
    pub features: BTreeMap<String, u32>,
    /// Outside items whose gating `cfg` names no feature.
    pub unattributed: u32,
}

/// The counted public items of a rustdoc document by full path: functions,
/// types and inherent methods. Trait-impl methods are not counted, and
/// rustdoc leaves `#[doc(hidden)]` items out of its JSON.
pub fn public_items(doc: &Value) -> BTreeMap<String, Entry> {
    let (Some(index), Some(paths)) = (doc["index"].as_object(), doc["paths"].as_object()) else {
        return BTreeMap::new();
    };
    let path_of = |id: &str| -> Option<String> {
        let segs = paths.get(id)?["path"].as_array()?;
        Some(
            segs.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("::"),
        )
    };
    let mut items: BTreeMap<String, Entry> = BTreeMap::new();
    for (id, item) in index {
        let Some(kind) = item["inner"].as_object().and_then(|i| i.keys().next()) else {
            continue;
        };
        let counted = COUNTED.contains(&kind.as_str());
        if item["visibility"] != "public" || !(counted || kind == "module") {
            continue;
        }
        if let Some(path) = path_of(id) {
            let features = cfg_features(item);
            items.insert(path, Entry { counted, features });
        }
    }
    for imp in index.values() {
        let inner = &imp["inner"]["impl"];
        if !inner.is_object() || inner["trait"].is_object() {
            continue;
        }
        let Some(owner) = inner["for"]["resolved_path"]["id"]
            .as_u64()
            .and_then(|id| path_of(&id.to_string()))
        else {
            continue;
        };
        let impl_features = cfg_features(imp);
        let members = inner["items"].as_array().into_iter().flatten();
        for member in members.filter_map(|id| index.get(&id.as_u64()?.to_string())) {
            let Some(name) = member["name"].as_str() else {
                continue;
            };
            if member["visibility"] != "public" || !member["inner"]["function"].is_object() {
                continue;
            }
            let mut features = cfg_features(member);
            features.extend(impl_features.iter().cloned());
            items.insert(
                format!("{owner}::{name}"),
                Entry {
                    counted: true,
                    features,
                },
            );
        }
    }
    inherit_features(&mut items);
    items.retain(|_, entry| entry.counted);
    items
}

/// Each item also carries the features its enclosing modules and types are
/// gated on, which rustdoc records on those items alone.
fn inherit_features(items: &mut BTreeMap<String, Entry>) {
    let own: BTreeMap<String, BTreeSet<String>> = items
        .iter()
        .filter(|(_, entry)| !entry.features.is_empty())
        .map(|(path, entry)| (path.clone(), entry.features.clone()))
        .collect();
    for (path, entry) in items.iter_mut() {
        let mut end = 0;
        while let Some(at) = path[end..].find("::") {
            end += at;
            if let Some(outer) = own.get(&path[..end]) {
                entry.features.extend(outer.iter().cloned());
            }
            end += 2;
        }
    }
}

/// The features a `#[cfg]` on `item` names. A `not(..)` names features the
/// item needs off, so the attribute attributes nothing.
fn cfg_features(item: &Value) -> BTreeSet<String> {
    const FEATURE: &str = "name: \"feature\", value: Some(\"";
    let mut features = BTreeSet::new();
    let attrs = item["attrs"].as_array().into_iter().flatten();
    for attr in attrs.filter_map(|a| a["other"].as_str().or(a.as_str())) {
        if !attr.contains("CfgTrace") || attr.contains("Not(") {
            continue;
        }
        for chunk in attr.split(FEATURE).skip(1) {
            features.extend(chunk.split_once('"').map(|(name, _)| name.to_string()));
        }
    }
    features
}

/// Every feature a build with `requested` (plus the defaults) turns on, from
/// the package's `[features]` table.
pub fn enabled_features(
    table: &BTreeMap<String, Vec<String>>,
    requested: Option<&str>,
) -> BTreeSet<String> {
    let mut enabled = BTreeSet::new();
    let mut pending: Vec<String> = requested
        .into_iter()
        .flat_map(|r| r.split(','))
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();
    pending.push("default".to_string());
    while let Some(feature) = pending.pop() {
        if !enabled.insert(feature.clone()) {
            continue;
        }
        for implied in table.get(&feature).into_iter().flatten() {
            if implied.starts_with("dep:") || implied.contains("?/") {
                continue;
            }
            pending.push(implied.split('/').next().unwrap_or(implied).to_string());
        }
    }
    enabled
}

/// Outside items of `reference` (a surface built with every feature), each
/// attributed through its `cfg` to the features of `declared` that `enabled`
/// lacks. A declared feature gating nothing is listed with 0.
pub fn from_cfg(
    reference: &BTreeMap<String, Entry>,
    gate: &BTreeSet<String>,
    enabled: &BTreeSet<String>,
    declared: &BTreeSet<String>,
) -> Outside {
    let mut out = Outside {
        reference: reference.len() as u32,
        features: declared
            .difference(enabled)
            .map(|f| (f.clone(), 0))
            .collect(),
        ..Outside::default()
    };
    for (path, entry) in reference {
        if gate.contains(path) {
            continue;
        }
        out.outside += 1;
        let gating: Vec<&String> = entry.features.difference(enabled).collect();
        out.unattributed += u32::from(gating.is_empty());
        for feature in gating {
            *out.features.entry(feature.clone()).or_default() += 1;
        }
    }
    out
}

/// Outside items from one surface per feature, each built with the gate's
/// features plus that feature, for a package whose features cannot all be
/// on together. An item present under several runs counts under each.
pub fn from_runs(gate: &BTreeSet<String>, runs: &BTreeMap<String, BTreeSet<String>>) -> Outside {
    let mut union = BTreeSet::new();
    let mut features = BTreeMap::new();
    for (feature, surface) in runs {
        let added: Vec<&String> = surface.difference(gate).collect();
        features.insert(feature.clone(), added.len() as u32);
        union.extend(added);
    }
    Outside {
        reference: (gate.len() + union.len()) as u32,
        outside: union.len() as u32,
        features,
        unattributed: 0,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn cfg(feature: &str) -> Value {
        json!([{ "other": format!(
            "#[attr = CfgTrace([NameValue {{ name: \"feature\", value: Some(\"{feature}\"), span: s (#0) }}])]"
        )}])
    }

    fn item(name: &str, kind: &str, attrs: Value) -> Value {
        json!({ "name": name, "visibility": "public", "attrs": attrs, "inner": { kind: {} } })
    }

    fn doc() -> Value {
        let mut m = item("m", "module", cfg("a"));
        m["inner"] = json!({ "module": {} });
        let method = |name: &str, attrs: Value| item(name, "function", attrs);
        json!({
            "index": {
                "1": item("always", "function", json!([])),
                "2": item("only_b", "function", cfg("b")),
                "3": m,
                "4": item("inner", "function", json!([])),
                "5": item("T", "struct", json!([])),
                "6": { "name": null, "visibility": "default", "attrs": cfg("c"),
                       "inner": { "impl": { "trait": null,
                           "for": { "resolved_path": { "id": 5 } }, "items": [7, 8] } } },
                "7": method("plain", json!([])),
                "8": method("extra", cfg("d")),
                "9": { "name": null, "visibility": "default", "attrs": [],
                       "inner": { "impl": { "trait": { "id": 1 },
                           "for": { "resolved_path": { "id": 5 } }, "items": [10] } } },
                "10": method("fmt", json!([])),
                "11": item("Konst", "constant", json!([])),
                "12": item("not_b", "function", json!([{ "other":
                    "#[attr = CfgTrace([Not(NameValue { name: \"feature\", value: Some(\"b\"), span: s (#0) }, s (#0))])]" }])),
            },
            "paths": {
                "1": { "path": ["k", "always"] },
                "2": { "path": ["k", "only_b"] },
                "3": { "path": ["k", "m"] },
                "4": { "path": ["k", "m", "inner"] },
                "5": { "path": ["k", "T"] },
                "11": { "path": ["k", "Konst"] },
                "12": { "path": ["k", "not_b"] },
            }
        })
    }

    fn set(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn items_inherit_the_features_of_their_module_and_impl() {
        let items = public_items(&doc());
        let features = |path: &str| -> Vec<&str> {
            items[path].features.iter().map(String::as_str).collect()
        };
        assert_eq!(features("k::always"), Vec::<&str>::new());
        assert_eq!(features("k::only_b"), ["b"]);
        assert_eq!(features("k::m::inner"), ["a"]);
        assert_eq!(features("k::T::plain"), ["c"]);
        assert_eq!(features("k::T::extra"), ["c", "d"]);
    }

    #[test]
    fn the_denominator_is_functions_types_and_inherent_methods() {
        let items = public_items(&doc());
        let paths: Vec<&str> = items.keys().map(String::as_str).collect();
        assert_eq!(
            paths,
            [
                "k::T",
                "k::T::extra",
                "k::T::plain",
                "k::always",
                "k::m::inner",
                "k::not_b",
                "k::only_b"
            ]
        );
    }

    #[test]
    fn a_negated_cfg_attributes_to_no_feature() {
        assert!(public_items(&doc())["k::not_b"].features.is_empty());
    }

    #[test]
    fn enabled_features_follow_the_table_and_the_defaults() {
        let table = BTreeMap::from([
            ("default".to_string(), vec!["std".to_string()]),
            ("std".to_string(), vec![]),
            ("full".to_string(), vec!["x/y".to_string(), "dep:z".to_string()]),
            ("x".to_string(), vec![]),
            ("opt".to_string(), vec!["w?/v".to_string()]),
            ("w".to_string(), vec![]),
        ]);
        let enabled = enabled_features(&table, Some("full"));
        assert_eq!(enabled, set(&["default", "std", "full", "x"]));
    }

    #[test]
    fn outside_items_are_attributed_to_their_ungated_features() {
        let items = public_items(&doc());
        let gate = set(&["k::always", "k::only_b", "k::m::inner"]);
        let declared = set(&["a", "b", "c", "d", "unused"]);
        let enabled = set(&["a", "b"]);
        let out = from_cfg(&items, &gate, &enabled, &declared);
        assert_eq!(out.reference, 7);
        assert_eq!(out.outside, 4);
        assert_eq!(
            out.features,
            BTreeMap::from([
                ("c".to_string(), 2),
                ("d".to_string(), 1),
                ("unused".to_string(), 0)
            ])
        );
        assert_eq!(out.unattributed, 2);
    }

    #[test]
    fn per_feature_runs_count_what_each_feature_adds() {
        let gate = set(&["k::a"]);
        let runs = BTreeMap::from([
            ("x".to_string(), set(&["k::a", "k::x1", "k::both"])),
            ("y".to_string(), set(&["k::a", "k::both"])),
            ("z".to_string(), set(&["k::a"])),
        ]);
        let out = from_runs(&gate, &runs);
        assert_eq!(out.outside, 2);
        assert_eq!(out.reference, 3);
        assert_eq!(
            out.features,
            BTreeMap::from([
                ("x".to_string(), 2),
                ("y".to_string(), 1),
                ("z".to_string(), 0)
            ])
        );
    }
}
