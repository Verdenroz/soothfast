//! Compatibility diffing between two generated AsyncAPI documents.
//!
//! The direction that decides what "required" costs is the operation's own
//! `action`, not a request/response split: a message this application
//! **sends** is data a consumer reads, so dropping a guaranteed field breaks
//! them; a message it **receives** is data a producer writes, so demanding a
//! new field breaks them instead. That is the same asymmetry OpenAPI has
//! between a response and a request body, read off a different key.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::compat::{Change, Direction, SchemaDiff, Severity, compare_keys, deref, sort};

/// Compare two generated documents, oldest first.
pub fn diff(old: &Value, new: &Value) -> Vec<Change> {
    let mut changes = Vec::new();
    let (old_ops, new_ops) = (operations(old), operations(new));

    compare_keys(
        &mut changes,
        &old_ops,
        &new_ops,
        "operation",
        |ch, at, o, n| {
            let (oa, na) = (action(o), action(n));
            if oa != na {
                ch.push(Change::new(
                    Severity::Breaking,
                    at,
                    format!("action {oa} -> {na}"),
                ));
                return; // The flow reversed; comparing payloads says nothing.
            }

            let (oc, nc) = (address(old, o), address(new, n));
            if oc != nc {
                ch.push(Change::new(
                    Severity::Breaking,
                    at,
                    format!("channel address {oc} -> {nc}"),
                ));
            }

            let dir = match na {
                // We publish it: a consumer reads it.
                "send" => Direction::Response,
                _ => Direction::Request,
            };
            match (payload(old, o), payload(new, n)) {
                (Some(op), Some(np)) => {
                    SchemaDiff::new(old, new).compare(ch, &format!("{at} payload"), &op, &np, dir);
                }
                (Some(_), None) => ch.push(Change::new(
                    Severity::Breaking,
                    format!("{at} payload"),
                    "message payload removed",
                )),
                (None, Some(_)) => ch.push(Change::new(
                    Severity::Additive,
                    format!("{at} payload"),
                    "message payload added",
                )),
                (None, None) => {}
            }
        },
    );

    sort(&mut changes);
    changes
}

fn operations(doc: &Value) -> BTreeMap<String, Value> {
    doc["operations"]
        .as_object()
        .map(|ops| ops.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

fn action(op: &Value) -> &str {
    op["action"].as_str().unwrap_or("?")
}

/// The address of the channel an operation points at — the address, not the
/// identifier, because renaming the identifier alone changes nothing on the
/// wire while changing the address moves the traffic.
fn address<'a>(doc: &'a Value, op: &'a Value) -> &'a str {
    follow(doc, &op["channel"])["address"]
        .as_str()
        .unwrap_or("?")
}

/// The payload schema of an operation's first message.
///
/// Generated operations carry exactly one message; a hand-edited document
/// with several is read for its first, which is the one a diff can speak
/// about without guessing at correspondence.
fn payload(doc: &Value, op: &Value) -> Option<Value> {
    let first = op["messages"].as_array()?.first()?;
    let message = follow(doc, first);
    let payload = &message["payload"];
    (!payload.is_null()).then(|| payload.clone())
}

/// Resolve a chain of `$ref`s — an operation's message reaches its payload
/// through the channel and then the components section.
fn follow<'a>(doc: &'a Value, mut node: &'a Value) -> &'a Value {
    // Bounded rather than cycle-tracked: a generated chain is two hops, and a
    // hand-edited cycle should stop rather than hang.
    for _ in 0..8 {
        if !node["$ref"].is_string() {
            break;
        }
        let next = deref(doc, node);
        if std::ptr::eq(next, node) {
            break; // Dangling reference: nothing further to resolve.
        }
        node = next;
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::is_compatible;
    use serde_json::json;

    /// A document with one operation whose message carries `payload`.
    fn doc(action: &str, payload: Value) -> Value {
        json!({
            "asyncapi": "3.0.0",
            "channels": {
                "orders": {
                    "address": "orders",
                    "messages": { "Order": { "$ref": "#/components/messages/Order" } },
                },
            },
            "operations": {
                "onOrder": {
                    "action": action,
                    "channel": { "$ref": "#/channels/orders" },
                    "messages": [ { "$ref": "#/channels/orders/messages/Order" } ],
                },
            },
            "components": { "messages": { "Order": { "payload": payload } } },
        })
    }

    fn object(props: Value, required: &[&str]) -> Value {
        json!({ "type": "object", "properties": props, "required": required })
    }

    #[test]
    fn an_unchanged_document_is_compatible_with_itself() {
        let d = doc(
            "send",
            object(json!({ "id": { "type": "string" } }), &["id"]),
        );
        assert!(is_compatible(&diff(&d, &d)));
        assert!(diff(&d, &d).is_empty());
    }

    #[test]
    fn a_sent_field_that_stops_being_guaranteed_breaks_consumers() {
        let old = doc(
            "send",
            object(json!({ "id": { "type": "string" } }), &["id"]),
        );
        let new = doc("send", object(json!({ "id": { "type": "string" } }), &[]));
        let changes = diff(&old, &new);
        assert!(!is_compatible(&changes), "got {changes:?}");
        assert!(changes[0].detail.contains("no longer guaranteed"));
    }

    #[test]
    fn a_new_field_on_a_sent_message_is_additive() {
        let old = doc(
            "send",
            object(json!({ "id": { "type": "string" } }), &["id"]),
        );
        let new = doc(
            "send",
            object(
                json!({ "id": { "type": "string" }, "note": { "type": "string" } }),
                &["id", "note"],
            ),
        );
        assert!(is_compatible(&diff(&old, &new)), "{:?}", diff(&old, &new));
    }

    #[test]
    fn the_same_new_field_on_a_received_message_breaks_producers() {
        let old = doc(
            "receive",
            object(json!({ "id": { "type": "string" } }), &["id"]),
        );
        let new = doc(
            "receive",
            object(
                json!({ "id": { "type": "string" }, "note": { "type": "string" } }),
                &["id", "note"],
            ),
        );
        let changes = diff(&old, &new);
        assert!(!is_compatible(&changes), "got {changes:?}");
        assert_eq!(changes[0].severity, Severity::Breaking);
    }

    #[test]
    fn reversing_the_flow_is_breaking_on_its_own() {
        let payload = object(json!({ "id": { "type": "string" } }), &["id"]);
        let changes = diff(&doc("send", payload.clone()), &doc("receive", payload));
        assert_eq!(changes.len(), 1, "got {changes:?}");
        assert!(changes[0].detail.contains("send -> receive"));
    }

    #[test]
    fn moving_a_channel_to_another_address_is_breaking() {
        let payload = object(json!({}), &[]);
        let old = doc("send", payload.clone());
        let mut new = doc("send", payload);
        new["channels"]["orders"]["address"] = json!("orders.v2");
        let changes = diff(&old, &new);
        assert_eq!(changes.len(), 1, "got {changes:?}");
        assert!(changes[0].detail.contains("orders -> orders.v2"));
    }

    #[test]
    fn a_removed_operation_breaks_consumers_and_a_new_one_does_not() {
        let payload = object(json!({}), &[]);
        let old = doc("send", payload.clone());
        let mut new = doc("send", payload);
        new["operations"] = json!({ "onOther": new["operations"]["onOrder"].clone() });
        let changes = diff(&old, &new);
        assert_eq!(changes.len(), 2, "got {changes:?}");
        assert_eq!(changes[0].severity, Severity::Breaking);
        assert_eq!(changes[0].at, "onOrder");
        assert_eq!(changes[1].severity, Severity::Additive);
    }

    #[test]
    fn payloads_resolve_through_the_channel_and_components_chain() {
        // The regression this guards: reading `messages[0]` without following
        // its two `$ref` hops finds no schema at all, and every payload change
        // then reads as "no change".
        let old = doc(
            "send",
            object(json!({ "id": { "type": "string" } }), &["id"]),
        );
        let new = doc(
            "send",
            object(json!({ "id": { "type": "integer" } }), &["id"]),
        );
        let changes = diff(&old, &new);
        assert_eq!(changes.len(), 1, "got {changes:?}");
        assert!(changes[0].detail.contains("string -> integer"));
    }

    #[test]
    fn a_dangling_reference_stops_rather_than_hanging() {
        let mut d = doc("send", object(json!({}), &[]));
        d["operations"]["onOrder"]["messages"] = json!([{ "$ref": "#/components/messages/Gone" }]);
        assert!(diff(&d, &d).is_empty());
    }
}
