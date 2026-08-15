//! Which fields of a JSON response actually carry data.
//!
//! A response is flattened to dotted paths; array indices collapse to `[]`
//! so five hundred candles produce one `candles[].close` entry. A path
//! counts as populated when at least one element under it holds a
//! non-empty value — `null`, `""`, `[]` and `{}` are all "no data", which
//! is what a dropped serde key or a hand-copy omission produces.

use std::collections::BTreeMap;

use serde_json::Value;

/// Flatten `value` into path → populated. Paths are dotted, arrays
/// collapse to `[]`, and a path already seen stays populated once any
/// occurrence carried data.
pub fn populate(value: &Value) -> BTreeMap<String, bool> {
    let mut map = BTreeMap::new();
    walk(value, String::new(), &mut map);
    map
}

fn walk(value: &Value, path: String, map: &mut BTreeMap<String, bool>) {
    match value {
        Value::Object(fields) => {
            record(&path, !fields.is_empty(), map);
            for (key, child) in fields {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                walk(child, child_path, map);
            }
        }
        Value::Array(items) => {
            record(&path, !items.is_empty(), map);
            let element_path = format!("{path}[]");
            for item in items {
                walk(item, element_path.clone(), map);
            }
        }
        Value::String(s) => record(&path, !s.is_empty(), map),
        Value::Null => record(&path, false, map),
        Value::Bool(_) | Value::Number(_) => record(&path, true, map),
    }
}

fn record(path: &str, populated: bool, map: &mut BTreeMap<String, bool>) {
    if path.is_empty() {
        return;
    }
    let entry = map.entry(path.to_string()).or_insert(false);
    *entry |= populated;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scalars_and_empties_classify() {
        let map = populate(&json!({
            "price": 1.5, "name": "", "note": null, "flag": false,
        }));
        assert!(map["price"]);
        assert!(!map["name"]);
        assert!(!map["note"]);
        assert!(map["flag"]);
    }

    #[test]
    fn array_indices_collapse_and_any_element_counts() {
        let map = populate(&json!({
            "candles": [{"close": null}, {"close": 3.0}],
        }));
        assert!(map["candles"]);
        assert!(map["candles[]"]);
        assert!(map["candles[].close"]);
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn all_null_under_an_array_stays_unpopulated() {
        let map = populate(&json!({"calls": [{"size": null}, {"size": null}]}));
        assert!(!map["calls[].size"]);
    }

    #[test]
    fn empty_containers_are_unpopulated_but_still_walked() {
        let map = populate(&json!({"meta": {}, "events": []}));
        assert!(!map["meta"]);
        assert!(!map["events"]);
    }
}
