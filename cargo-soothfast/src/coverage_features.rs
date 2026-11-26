//! `coverage features`: public items behind cargo features that no gate run
//! compiles, per feature. Coverage is computed under the gate's feature set,
//! so an item behind any other feature is absent from every denominator.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use serde_json::{Value, json};

use crate::config::{self, Group};
use crate::invoke::{self, Visibility};

/// Whether a rustdoc item kind belongs in the denominator.
fn counted(kind: &str) -> bool {
    ["function", "struct", "enum", "union", "trait", "type_alias"].contains(&kind)
}

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
    /// Outside items present only while a gate-enabled feature is off.
    pub off_under_feature: u32,
    /// Outside items whose gating `cfg` the parser cannot read.
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
        let counted = counted(kind);
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
        if !items.get(&owner).is_some_and(|e| e.counted) {
            continue;
        }
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

/// Each item also carries the gating of its enclosing modules and types,
/// which rustdoc records on those items alone.
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

/// The features a `#[cfg]` on `item` needs on. A feature under an odd number
/// of `not(..)` is needed off and is left out.
fn cfg_features(item: &Value) -> BTreeSet<String> {
    const FEATURE: &str = "name: \"feature\", value: Some(\"";
    let mut on = BTreeSet::new();
    let attrs = item["attrs"].as_array().into_iter().flatten();
    for attr in attrs.filter_map(|a| a["other"].as_str().or(a.as_str())) {
        if !attr.contains("CfgTrace") {
            continue;
        }
        let mut nots: Vec<bool> = Vec::new();
        let mut at = 0;
        while at < attr.len() {
            let rest = &attr[at..];
            if let Some(after) = rest.strip_prefix(FEATURE) {
                let name = after.split('"').next().unwrap_or_default();
                if nots.iter().filter(|not| **not).count() % 2 == 0 {
                    on.insert(name.to_string());
                }
                at += FEATURE.len() + name.len();
                continue;
            }
            let Some(c) = rest.chars().next() else { break };
            match c {
                '(' => nots.push(attr[..at].ends_with("Not")),
                '[' | '{' => nots.push(false),
                ')' | ']' | '}' => {
                    nots.pop();
                }
                _ => {}
            }
            at += c.len_utf8();
        }
    }
    on
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
    bare: &BTreeSet<String>,
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
    let reference_paths = reference.keys().collect();
    out.count_off_under_feature(bare, gate, &reference_paths);
    out
}

impl Outside {
    /// Items of the surface built with no features that neither the gate nor
    /// the reference has: they exist only while some feature is off, which a
    /// gate that turns it on never compiles.
    fn count_off_under_feature(
        &mut self,
        bare: &BTreeSet<String>,
        gate: &BTreeSet<String>,
        reference: &BTreeSet<&String>,
    ) {
        let missed = bare
            .iter()
            .filter(|path| !gate.contains(*path) && !reference.contains(path))
            .count() as u32;
        self.off_under_feature = missed;
        self.outside += missed;
        self.reference += missed;
    }
}

/// Outside items from one surface per feature, each built with the gate's
/// features plus that feature, for a package whose features cannot all be
/// on together. An item present under several runs counts under each.
pub fn from_runs(
    gate: &BTreeSet<String>,
    runs: &BTreeMap<String, BTreeSet<String>>,
    bare: &BTreeSet<String>,
) -> Outside {
    let mut union = BTreeSet::new();
    let mut features = BTreeMap::new();
    for (feature, surface) in runs {
        let added: Vec<&String> = surface.difference(gate).collect();
        features.insert(feature.clone(), added.len() as u32);
        union.extend(added);
    }
    let mut out = Outside {
        reference: (gate.len() + union.len()) as u32,
        outside: union.len() as u32,
        features,
        ..Outside::default()
    };
    let reference_paths = union.iter().copied().collect();
    out.count_off_under_feature(bare, gate, &reference_paths);
    out
}

/// `coverage features -p PKG [--json] [--max-outside N]`.
pub fn run(args: &[String]) -> i32 {
    let mut pkg = None;
    let mut json = false;
    let mut max_outside: Option<u32> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-p" | "--package" => pkg = it.next().cloned(),
            "--json" => json = true,
            "--max-outside" => match it.next().and_then(|v| v.parse().ok()) {
                Some(n) => max_outside = Some(n),
                None => {
                    eprintln!("soothfast: --max-outside takes a whole number");
                    return 2;
                }
            },
            other => {
                eprintln!("soothfast: unknown coverage features arg {other:?}");
                return 2;
            }
        }
    }
    let Some(pkg) = pkg else {
        eprintln!("soothfast: coverage features requires -p PKG");
        return 2;
    };
    let gate_features = match config::defaults(std::slice::from_ref(&pkg), Group::Gate) {
        Ok(settings) => settings.features,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 2;
        }
    };
    let (outside, route) = match measure(&pkg, gate_features.as_deref()) {
        Ok(measured) => measured,
        Err(e) => {
            eprintln!("soothfast: {e}");
            return 1;
        }
    };
    if json {
        println!(
            "{}",
            render_json(&pkg, gate_features.as_deref(), &outside, route)
        );
    } else {
        print!(
            "{}",
            render_text(&pkg, gate_features.as_deref(), &outside, route)
        );
    }
    match max_outside {
        Some(max) if outside.outside > max => {
            eprintln!(
                "coverage features: FAILED ({} outside item(s) > allowed {max})",
                outside.outside
            );
            1
        }
        _ => 0,
    }
}

/// What lies outside `pkg`'s gate features, as the text and the JSON of
/// `coverage features`.
pub(crate) fn section(pkg: &str) -> Result<(String, Value), String> {
    let gate = config::defaults(&[pkg.to_string()], Group::Gate)?.features;
    let (outside, route) = measure(pkg, gate.as_deref())?;
    Ok((
        render_text(pkg, gate.as_deref(), &outside, route),
        render_json(pkg, gate.as_deref(), &outside, route),
    ))
}

/// How the reference surface was obtained.
#[derive(Clone, Copy)]
enum Route {
    AllFeatures,
    PerFeature,
}

fn measure(pkg: &str, gate_features: Option<&str>) -> Result<(Outside, Route), String> {
    let table = feature_table(pkg)?;
    let enabled = enabled_features(&table, gate_features);
    let declared: BTreeSet<String> = table.keys().cloned().collect();
    let (gate_doc, _) = invoke::rustdoc_json_in(pkg, None, gate_features, Visibility::Public)
        .map_err(|e| e.to_string())?;
    let gate: BTreeSet<String> = public_items(&gate_doc).into_keys().collect();
    let bare: BTreeSet<String> = if enabled.iter().any(|f| table.contains_key(f)) {
        let (doc, _) = invoke::rustdoc_json_exactly(pkg, None, "", Visibility::Public)
            .map_err(|e| e.to_string())?;
        public_items(&doc).into_keys().collect()
    } else {
        gate.clone()
    };
    match invoke::rustdoc_json_all(pkg, None, Visibility::Public) {
        Ok((doc, _)) => {
            let outside = from_cfg(&public_items(&doc), &gate, &enabled, &declared, &bare);
            return Ok((outside, Route::AllFeatures));
        }
        Err(e) => eprintln!("soothfast: --all-features rustdoc failed: {e}"),
    }
    let mut runs = BTreeMap::new();
    for feature in declared.difference(&enabled) {
        let features = match gate_features {
            Some(g) => format!("{g},{feature}"),
            None => feature.clone(),
        };
        let (doc, _) = invoke::rustdoc_json_in(pkg, None, Some(&features), Visibility::Public)
            .map_err(|e| format!("rustdoc with feature {feature}: {e}"))?;
        runs.insert(feature.clone(), public_items(&doc).into_keys().collect());
    }
    Ok((from_runs(&gate, &runs, &bare), Route::PerFeature))
}

/// The `[features]` table of workspace package `pkg`.
fn feature_table(pkg: &str) -> Result<BTreeMap<String, Vec<String>>, String> {
    let root = invoke::workspace_root().map_err(|e| e.to_string())?;
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let meta: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let package = (meta["packages"].as_array().into_iter().flatten())
        .find(|p| p["name"] == pkg)
        .ok_or_else(|| format!("no workspace package {pkg:?}"))?;
    Ok((package["features"].as_object().into_iter().flatten())
        .map(|(name, implied)| {
            let implied = implied.as_array().into_iter().flatten();
            (
                name.clone(),
                implied
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
            )
        })
        .collect())
}

fn render_text(pkg: &str, gate: Option<&str>, out: &Outside, route: Route) -> String {
    let mut text = format!(
        "coverage features: {pkg} (gate features: {})\n",
        gate.unwrap_or("none")
    );
    if matches!(route, Route::PerFeature) {
        text.push_str(
            "note: --all-features rustdoc failed; each feature was built on top of the gate's set, one rustdoc run per feature\n",
        );
    }
    for (feature, items) in &out.features {
        text.push_str(&format!("{feature:<24} {items} public item(s)\n"));
    }
    if out.off_under_feature > 0 {
        text.push_str(&format!(
            "{:<24} {} public item(s)\n",
            "off under a feature", out.off_under_feature
        ));
    }
    if out.unattributed > 0 {
        text.push_str(&format!(
            "{:<24} {} public item(s)\n",
            "(cfg not read)", out.unattributed
        ));
        text.push_str(&format!(
            "warning: items whose cfg names no feature the parser can read (rustdoc JSON format {})\n",
            invoke::RUSTDOC_FORMAT_VERSION
        ));
    }
    text.push_str(&format!(
        "coverage features: {}/{} public item(s) sit outside the gate's feature set\n",
        out.outside, out.reference
    ));
    text
}

fn render_json(pkg: &str, gate: Option<&str>, out: &Outside, route: Route) -> Value {
    let features: Vec<Value> = (out.features.iter())
        .map(|(feature, items)| json!({ "feature": feature, "items": items }))
        .collect();
    json!({
        "package": pkg,
        "gate_features": gate,
        "reference": match route {
            Route::AllFeatures => "all-features",
            Route::PerFeature => "per-feature",
        },
        "public_items": out.reference,
        "outside": out.outside,
        "features": features,
        "off_under_feature": out.off_under_feature,
        "unattributed": out.unattributed,
    })
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
        let features =
            |path: &str| -> Vec<&str> { items[path].features.iter().map(String::as_str).collect() };
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
    fn cfg_attributes_parse_as_rustdoc_writes_them() {
        assert_eq!(
            invoke::RUSTDOC_FORMAT_VERSION,
            61,
            "recapture the attrs below"
        );
        let attrs = |text: &str| json!({ "attrs": [{ "other": text }] });
        let single = attrs(
            r#"#[attr = CfgTrace([NameValue { name: "feature", value: Some("x"), span: src/lib.rs:2:7: 2:20 (#0) }])]"#,
        );
        let all = attrs(
            r#"#[attr = CfgTrace([All([NameValue { name: "feature", value: Some("a"), span: src/lib.rs:4:11: 4:24 (#0) }, NameValue { name: "feature", value: Some("b"), span: src/lib.rs:4:26: 4:39 (#0) }], src/lib.rs:4:10: 4:40 (#0))])]"#,
        );
        let any = attrs(
            r#"#[attr = CfgTrace([Any([NameValue { name: "feature", value: Some("a"), span: src/lib.rs:16:11: 16:24 (#0) }, NameValue { name: "feature", value: Some("b"), span: src/lib.rs:16:26: 16:39 (#0) }], src/lib.rs:16:10: 16:40 (#0))])]"#,
        );
        let not = attrs(
            r#"#[attr = CfgTrace([Not(NameValue { name: "feature", value: Some("y"), span: src/lib.rs:18:11: 18:26 (#0) }, src/lib.rs:18:10: 18:27 (#0))])]"#,
        );
        assert_eq!(cfg_features(&single), set(&["x"]));
        assert_eq!(cfg_features(&all), set(&["a", "b"]));
        assert_eq!(cfg_features(&any), set(&["a", "b"]));
        assert_eq!(cfg_features(&not), set(&[]));
        let nested = attrs(
            r#"#[attr = CfgTrace([Not(Any([NameValue { name: "feature", value: Some("a"), span: s (#0) }], s (#0)), s (#0))])]"#,
        );
        assert_eq!(cfg_features(&nested), set(&[]));
        let mixed = attrs(
            r#"#[attr = CfgTrace([All([NameValue { name: "feature", value: Some("a"), span: s (#0) }, Not(NameValue { name: "feature", value: Some("b"), span: s (#0) }, s (#0)), Not(Any([NameValue { name: "feature", value: Some("c"), span: s (#0) }], s (#0)), s (#0))], s (#0))])]"#,
        );
        assert_eq!(cfg_features(&mixed), set(&["a"]));
    }

    #[test]
    fn a_negated_cfg_attributes_to_no_feature() {
        let items = public_items(&doc());
        assert!(items["k::not_b"].features.is_empty());
    }

    #[test]
    fn enabled_features_follow_the_table_and_the_defaults() {
        let table = BTreeMap::from([
            ("default".to_string(), vec!["std".to_string()]),
            ("std".to_string(), vec![]),
            (
                "full".to_string(),
                vec!["x/y".to_string(), "dep:z".to_string()],
            ),
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
        let gate = set(&["k::always", "k::only_b", "k::m::inner", "k::not_b"]);
        let declared = set(&["a", "b", "c", "d", "unused"]);
        let enabled = set(&["a", "b"]);
        let out = from_cfg(
            &items,
            &gate,
            &enabled,
            &declared,
            &set(&["k::always", "k::nb"]),
        );
        assert_eq!(out.reference, 8);
        assert_eq!(out.outside, 4);
        assert_eq!(
            out.features,
            BTreeMap::from([
                ("c".to_string(), 2),
                ("d".to_string(), 1),
                ("unused".to_string(), 0)
            ])
        );
        assert_eq!(out.off_under_feature, 1);
        assert_eq!(out.unattributed, 1);
    }

    #[test]
    fn per_feature_runs_count_what_each_feature_adds() {
        let gate = set(&["k::a"]);
        let runs = BTreeMap::from([
            ("x".to_string(), set(&["k::a", "k::x1", "k::both"])),
            ("y".to_string(), set(&["k::a", "k::both"])),
            ("z".to_string(), set(&["k::a"])),
        ]);
        let out = from_runs(&gate, &runs, &set(&["k::a", "k::nb"]));
        assert_eq!(out.outside, 3);
        assert_eq!(out.reference, 4);
        assert_eq!(out.off_under_feature, 1);
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
