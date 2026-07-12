//! Consumer compatibility: what changed, and whether it breaks anyone.
//!
//! Once a spec is generated it can no longer disagree with the code, so the
//! question worth gating shifts: not "does this match?" but "did this break
//! the people calling it?". Every generated dialect asks that same question,
//! and the payload half of the answer is always a JSON Schema comparison, so
//! it lives here rather than in any one dialect.
//!
//! Requiredness means opposite things in the two directions, which is the
//! subtlety the whole module turns on:
//!
//! - **Request** (consumer → us): growing *stricter* breaks them. A newly
//!   required field is breaking; dropping a requirement is not.
//! - **Response** (us → consumer): providing *less* breaks them. A field that
//!   stops being guaranteed is breaking; a new one is not.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// Whether a change can break an existing consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Safe to release: consumers written against the old spec still work.
    Additive,
    /// Breaks consumers written against the old spec.
    Breaking,
}

/// One difference between two versions of a spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub severity: Severity,
    /// Where it happened, e.g. `POST /items requestBody.qty`.
    pub at: String,
    pub detail: String,
}

impl Change {
    pub fn new(severity: Severity, at: impl Into<String>, detail: impl Into<String>) -> Self {
        Change {
            severity,
            at: at.into(),
            detail: detail.into(),
        }
    }
}

/// True when nothing in the diff would break an existing consumer.
pub fn is_compatible(changes: &[Change]) -> bool {
    !changes.iter().any(|c| c.severity == Severity::Breaking)
}

/// Order a diff for reporting: breaking first, then by location, so output is
/// stable between runs.
pub fn sort(changes: &mut [Change]) {
    changes.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.at.cmp(&b.at))
            .then_with(|| a.detail.cmp(&b.detail))
    });
}

/// Which way the data flows, which decides what "required" costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Consumer-authored data arriving at us.
    Request,
    /// Data we hand to a consumer.
    Response,
}

/// Compares schemas belonging to two revisions of a document.
///
/// Both documents are held because a `$ref` only means anything relative to
/// the document it came from: the same pointer can name different shapes on
/// either side of the comparison.
pub struct SchemaDiff<'a> {
    old_doc: &'a Value,
    new_doc: &'a Value,
}

impl<'a> SchemaDiff<'a> {
    /// Compare schemas from `old_doc` against schemas from `new_doc`.
    pub fn new(old_doc: &'a Value, new_doc: &'a Value) -> Self {
        SchemaDiff { old_doc, new_doc }
    }

    /// Compare two schemas in a known data-flow direction, appending whatever
    /// differs to `changes`.
    pub fn compare(
        &self,
        changes: &mut Vec<Change>,
        at: &str,
        old: &Value,
        new: &Value,
        dir: Direction,
    ) {
        self.walk(changes, at, old, new, dir, &mut BTreeSet::new());
    }

    fn walk(
        &self,
        changes: &mut Vec<Change>,
        at: &str,
        old: &Value,
        new: &Value,
        dir: Direction,
        seen: &mut BTreeSet<(String, String)>,
    ) {
        // Self-referential schemas would otherwise recurse forever.
        let pair = (
            old["$ref"].as_str().unwrap_or_default().to_string(),
            new["$ref"].as_str().unwrap_or_default().to_string(),
        );
        if !pair.0.is_empty() || !pair.1.is_empty() {
            if seen.contains(&pair) {
                return;
            }
            seen.insert(pair);
        }

        let old = deref(self.old_doc, old);
        let new = deref(self.new_doc, new);

        let (ot, nt) = (&old["type"], &new["type"]);
        if ot != nt && !(ot.is_null() && nt.is_null()) {
            changes.push(Change::new(
                Severity::Breaking,
                at,
                format!("type {} -> {}", render(ot), render(nt)),
            ));
            return; // Field-level diffs are meaningless once the type moved.
        }

        // Arrays: compare element schemas.
        if ot == "array" {
            self.walk(
                changes,
                &format!("{at}[]"),
                &old["items"],
                &new["items"],
                dir,
                seen,
            );
            return;
        }

        let empty = serde_json::Map::new();
        let old_props = old["properties"].as_object().unwrap_or(&empty);
        let new_props = new["properties"].as_object().unwrap_or(&empty);
        let old_req = required_set(old);
        let new_req = required_set(new);

        let names: BTreeSet<&String> = old_props.keys().chain(new_props.keys()).collect();
        for name in names {
            let where_ = format!("{at}.{name}");
            match (old_props.get(name), new_props.get(name)) {
                (Some(_), None) => {
                    // Losing a response field breaks readers; losing a request
                    // field means data callers still send is now ignored.
                    changes.push(Change::new(Severity::Breaking, where_, "property removed"));
                }
                (None, Some(_)) => {
                    let newly_required = new_req.contains(name.as_str());
                    let severity = match (dir, newly_required) {
                        // A new required request field breaks every caller.
                        (Direction::Request, true) => Severity::Breaking,
                        _ => Severity::Additive,
                    };
                    changes.push(Change::new(severity, where_, "property added"));
                }
                (Some(o), Some(n)) => {
                    match (
                        old_req.contains(name.as_str()),
                        new_req.contains(name.as_str()),
                    ) {
                        (false, true) if dir == Direction::Request => changes.push(Change::new(
                            Severity::Breaking,
                            &where_,
                            "became required",
                        )),
                        (true, false) if dir == Direction::Response => changes.push(Change::new(
                            Severity::Breaking,
                            &where_,
                            "no longer guaranteed",
                        )),
                        (false, true) => changes.push(Change::new(
                            Severity::Additive,
                            &where_,
                            "became required",
                        )),
                        (true, false) => changes.push(Change::new(
                            Severity::Additive,
                            &where_,
                            "became optional",
                        )),
                        _ => {}
                    }
                    self.walk(changes, &where_, o, n, dir, seen);
                }
                (None, None) => {}
            }
        }

        compare_enum(changes, at, old, new);
    }
}

/// A removed accepted value breaks callers that still send or expect it.
fn compare_enum(changes: &mut Vec<Change>, at: &str, old: &Value, new: &Value) {
    let values = |v: &Value| -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        if let Some(c) = v.get("const") {
            out.insert(render(c));
        }
        if let Some(list) = v["enum"].as_array() {
            out.extend(list.iter().map(render));
        }
        out
    };
    let (o, n) = (values(old), values(new));
    if o.is_empty() && n.is_empty() {
        return;
    }
    for gone in o.difference(&n) {
        changes.push(Change::new(
            Severity::Breaking,
            at,
            format!("value {gone} no longer accepted"),
        ));
    }
    for added in n.difference(&o) {
        changes.push(Change::new(
            Severity::Additive,
            at,
            format!("value {added} added"),
        ));
    }
}

/// Follow a local `$ref` to the schema it names, once.
///
/// Resolution is by JSON Pointer rather than by a fixed `components/schemas`
/// prefix: dialects disagree on where shared schemas live (`#/$defs/X` inside
/// one tool's schema, `#/components/schemas/X` at the document root), and the
/// pointer already says which.
pub fn deref<'a>(doc: &'a Value, schema: &'a Value) -> &'a Value {
    let Some(pointer) = schema["$ref"].as_str() else {
        return schema;
    };
    match resolve_pointer(doc, pointer) {
        Some(target) => target,
        None => schema,
    }
}

/// Walk a `#/a/b/c` JSON Pointer from a document root.
fn resolve_pointer<'a>(doc: &'a Value, pointer: &str) -> Option<&'a Value> {
    let path = pointer.strip_prefix('#')?;
    let mut node = doc;
    for raw in path.split('/').skip(1) {
        if raw.is_empty() {
            continue;
        }
        let token = raw.replace("~1", "/").replace("~0", "~");
        node = node.get(&token)?;
    }
    Some(node)
}

/// The `required` names of an object schema.
pub fn required_set(schema: &Value) -> BTreeSet<&str> {
    schema["required"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default()
}

/// A JSON value as it should read inside a diff message.
pub fn render(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "none".into(),
        other => other.to_string(),
    }
}

/// Compare two maps of named entries, reporting bare presence changes and
/// handing matched pairs to `both`.
///
/// Added and removed entries mean the same thing in every dialect — a tool, a
/// channel, an operation or a field that appears or disappears — so only the
/// wording and the severity of a *removal* differ between callers.
pub fn compare_keys<F>(
    changes: &mut Vec<Change>,
    old: &BTreeMap<String, Value>,
    new: &BTreeMap<String, Value>,
    noun: &str,
    mut both: F,
) where
    F: FnMut(&mut Vec<Change>, &str, &Value, &Value),
{
    let keys: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    for key in keys {
        match (old.get(key), new.get(key)) {
            (Some(_), None) => changes.push(Change::new(
                Severity::Breaking,
                key.clone(),
                format!("{noun} removed"),
            )),
            (None, Some(_)) => changes.push(Change::new(
                Severity::Additive,
                key.clone(),
                format!("{noun} added"),
            )),
            (Some(o), Some(n)) => both(changes, key, o, n),
            (None, None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn diff_schemas(old: &Value, new: &Value, dir: Direction) -> Vec<Change> {
        let empty = json!({});
        let mut changes = Vec::new();
        SchemaDiff::new(&empty, &empty).compare(&mut changes, "at", old, new, dir);
        changes
    }

    #[test]
    fn a_new_required_request_field_breaks_callers() {
        let old = json!({ "type": "object", "properties": { "a": { "type": "string" } } });
        let new = json!({ "type": "object",
                          "properties": { "a": { "type": "string" }, "b": { "type": "string" } },
                          "required": ["b"] });
        let changes = diff_schemas(&old, &new, Direction::Request);
        assert_eq!(changes[0].severity, Severity::Breaking);
        assert_eq!(changes[0].at, "at.b");
    }

    #[test]
    fn the_same_new_field_on_a_response_is_additive() {
        let old = json!({ "type": "object", "properties": { "a": { "type": "string" } } });
        let new = json!({ "type": "object",
                          "properties": { "a": { "type": "string" }, "b": { "type": "string" } },
                          "required": ["b"] });
        let changes = diff_schemas(&old, &new, Direction::Response);
        assert_eq!(changes[0].severity, Severity::Additive);
    }

    #[test]
    fn a_response_field_that_stops_being_guaranteed_breaks_readers() {
        let old = json!({ "type": "object", "properties": { "a": { "type": "string" } },
                          "required": ["a"] });
        let new = json!({ "type": "object", "properties": { "a": { "type": "string" } } });
        let changes = diff_schemas(&old, &new, Direction::Response);
        assert_eq!(changes[0].severity, Severity::Breaking);
        assert_eq!(changes[0].detail, "no longer guaranteed");
    }

    #[test]
    fn refs_resolve_through_a_json_pointer_wherever_defs_live() {
        let old_doc = json!({ "$defs": { "Item": { "type": "object",
                                                   "properties": { "id": { "type": "string" } } } } });
        let new_doc = json!({ "$defs": { "Item": { "type": "object",
                                                   "properties": { "id": { "type": "integer" } } } } });
        let r = json!({ "$ref": "#/$defs/Item" });
        let mut changes = Vec::new();
        SchemaDiff::new(&old_doc, &new_doc).compare(
            &mut changes,
            "Item",
            &r,
            &r,
            Direction::Response,
        );
        assert_eq!(changes.len(), 1, "got {changes:?}");
        assert_eq!(changes[0].detail, "type string -> integer");
    }

    #[test]
    fn a_ref_that_resolves_nowhere_is_left_as_written() {
        let doc = json!({});
        let schema = json!({ "$ref": "#/components/schemas/Gone" });
        assert_eq!(deref(&doc, &schema), &schema);
    }

    #[test]
    fn a_self_referential_schema_terminates() {
        let doc = json!({ "$defs": { "Node": { "type": "object",
                                               "properties": { "next": { "$ref": "#/$defs/Node" } } } } });
        let r = json!({ "$ref": "#/$defs/Node" });
        let mut changes = Vec::new();
        SchemaDiff::new(&doc, &doc).compare(&mut changes, "Node", &r, &r, Direction::Response);
        assert!(changes.is_empty(), "got {changes:?}");
    }

    #[test]
    fn a_removed_enum_value_breaks_consumers() {
        let old = json!({ "type": "string", "enum": ["a", "b"] });
        let new = json!({ "type": "string", "enum": ["a"] });
        let changes = diff_schemas(&old, &new, Direction::Response);
        assert_eq!(changes[0].severity, Severity::Breaking);
        assert!(changes[0].detail.contains('b'));
    }

    #[test]
    fn breaking_changes_sort_ahead_of_additive_ones() {
        let mut changes = vec![
            Change::new(Severity::Additive, "z", "added"),
            Change::new(Severity::Breaking, "a", "removed"),
        ];
        sort(&mut changes);
        assert_eq!(changes[0].severity, Severity::Breaking);
        assert!(!is_compatible(&changes));
    }

    #[test]
    fn presence_changes_report_with_the_callers_noun() {
        let old: BTreeMap<String, Value> = [("gone".to_string(), json!({}))].into();
        let new: BTreeMap<String, Value> = [("fresh".to_string(), json!({}))].into();
        let mut changes = Vec::new();
        compare_keys(&mut changes, &old, &new, "tool", |_, _, _, _| {});
        sort(&mut changes);
        assert_eq!(changes[0].detail, "tool removed");
        assert_eq!(changes[0].severity, Severity::Breaking);
        assert_eq!(changes[1].detail, "tool added");
    }
}
