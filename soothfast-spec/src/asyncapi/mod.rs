//! AsyncAPI 3.0 documents assembled from inferred route shapes.
//!
//! 3.0 rather than 2.x because 3.0 separates channels (where messages travel)
//! from operations (what this application does with them), which is the
//! distinction a handler annotation actually carries: one channel can be both
//! produced to and consumed from, and 2.x had nowhere to say so.
//!
//! # Which way is "send"?
//!
//! 3.0's `action` is written from the *application's* point of view: `send`
//! means this application emits the message. 2.x's channel-level
//! `publish`/`subscribe` keys were written from the *client's*, and so mean
//! the opposite of what they look like. Both vocabularies are accepted on
//! `#[route(method = ...)]` and both map to the same thing:
//!
//! | method | action | meaning |
//! |---|---|---|
//! | `SEND`, `SUBSCRIBE` | `send` | this application publishes the message |
//! | `RECEIVE`, `PUBLISH` | `receive` | this application consumes the message |
//!
//! `SUBSCRIBE`/`PUBLISH` keep their 2.x sense so that one annotation
//! reconciles against a hand-authored 2.x file (which
//! [`crate::providers`] reads) *and* generates a 3.0 file that means the same
//! thing. `SEND`/`RECEIVE` say it without the historical inversion.

pub mod diff;

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::dialect::{Document, Info, Operation, unknown_method};
use crate::schema::route_sig::RouteShape;

/// What this application does with a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Send,
    Receive,
}

impl Action {
    fn parse(method: &str) -> Option<Action> {
        match method.to_ascii_uppercase().as_str() {
            "SEND" | "SUBSCRIBE" => Some(Action::Send),
            "RECEIVE" | "PUBLISH" => Some(Action::Receive),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Action::Send => "send",
            Action::Receive => "receive",
        }
    }
}

/// Assemble an AsyncAPI 3.0 document.
pub fn document(info: &Info, ops: &[Operation]) -> Document {
    let mut doc = Document::default();
    // Channel identifier → (address, messages).
    let mut channels: BTreeMap<String, Channel> = BTreeMap::new();
    let mut operations: BTreeMap<String, Value> = BTreeMap::new();
    let mut messages: BTreeMap<String, Value> = BTreeMap::new();
    let mut schemas: BTreeMap<String, Value> = BTreeMap::new();

    for op in ops {
        let Some(action) = Action::parse(&op.method) else {
            doc.conflicts.push(unknown_method(
                op,
                "AsyncAPI",
                "SEND, RECEIVE, PUBLISH, SUBSCRIBE",
            ));
            continue;
        };
        if operations.contains_key(&op.operation_id) {
            doc.conflicts.push(format!(
                "operation `{}` is declared by more than one route",
                op.operation_id
            ));
            continue;
        }

        for (name, schema) in &op.shape.components {
            if let Some(existing) = schemas.get(name)
                && existing != schema
            {
                doc.conflicts.push(format!(
                    "schema `{name}` has two different definitions \
                         (second seen via operation `{}`)",
                    op.operation_id
                ));
                continue;
            }
            schemas.insert(name.clone(), schema.clone());
        }

        let key = channel_key(&op.path);
        let channel = channels.entry(key.clone()).or_insert_with(|| Channel {
            address: op.path.clone(),
            messages: BTreeMap::new(),
            parameters: address_parameters(&op.path, &op.shape),
        });
        if channel.address != op.path {
            doc.conflicts.push(format!(
                "channels `{}` and `{}` collapse to the same identifier `{key}`",
                channel.address, op.path
            ));
            continue;
        }

        let (message_name, payload) = message_for(op, action);
        messages
            .entry(message_name.clone())
            .or_insert_with(|| json!({ "payload": payload }));
        channel.messages.insert(
            message_name.clone(),
            json!({ "$ref": format!("#/components/messages/{message_name}") }),
        );

        note_unusable_parameters(op, &mut doc);
        operations.insert(
            op.operation_id.clone(),
            operation_object(op, action, &key, &message_name),
        );
    }

    let mut out = serde_json::Map::new();
    out.insert("asyncapi".into(), json!("3.0.0"));
    out.insert("info".into(), info_object(info));
    let servers = servers_object(info, &mut doc);
    if !servers.is_empty() {
        out.insert("servers".into(), Value::Object(servers));
    }
    out.insert(
        "channels".into(),
        Value::Object(
            channels
                .into_iter()
                .map(|(k, c)| (k, c.into_value()))
                .collect(),
        ),
    );
    out.insert(
        "operations".into(),
        Value::Object(operations.into_iter().collect()),
    );

    let mut components = serde_json::Map::new();
    if !messages.is_empty() {
        components.insert(
            "messages".into(),
            Value::Object(messages.into_iter().collect()),
        );
    }
    if !schemas.is_empty() {
        components.insert(
            "schemas".into(),
            Value::Object(schemas.into_iter().collect()),
        );
    }
    if !components.is_empty() {
        out.insert("components".into(), Value::Object(components));
    }

    doc.value = Value::Object(out);
    doc
}

/// One channel under construction.
struct Channel {
    address: String,
    messages: BTreeMap<String, Value>,
    parameters: BTreeMap<String, Value>,
}

impl Channel {
    fn into_value(self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("address".into(), json!(self.address));
        m.insert(
            "messages".into(),
            Value::Object(self.messages.into_iter().collect()),
        );
        if !self.parameters.is_empty() {
            m.insert(
                "parameters".into(),
                Value::Object(self.parameters.into_iter().collect()),
            );
        }
        Value::Object(m)
    }
}

fn info_object(info: &Info) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("title".into(), json!(info.title));
    m.insert("version".into(), json!(info.version));
    if let Some(d) = &info.description {
        m.insert("description".into(), json!(d));
    }
    Value::Object(m)
}

/// AsyncAPI splits a server into host and protocol, both required, so a bare
/// hostname is not enough — the scheme is where the protocol comes from.
fn servers_object(info: &Info, doc: &mut Document) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for url in &info.servers {
        let Some((protocol, rest)) = url.split_once("://") else {
            doc.notes.push(format!(
                "server `{url}` has no scheme, so AsyncAPI's required \
                 `protocol` cannot be derived; write it as a URL \
                 (`kafka://host:9092`) to have it emitted"
            ));
            continue;
        };
        let (host, path) = match rest.split_once('/') {
            Some((h, p)) => (h, p),
            None => (rest, ""),
        };
        let mut server = serde_json::Map::new();
        server.insert("host".into(), json!(host));
        server.insert("protocol".into(), json!(protocol));
        if !path.is_empty() {
            server.insert("pathname".into(), json!(format!("/{path}")));
        }
        let base = identifier(host.split(['.', ':']).next().unwrap_or(host));
        let mut name = if base.is_empty() {
            "server".into()
        } else {
            base
        };
        let mut n = 2;
        while out.contains_key(&name) {
            name = format!("{name}{n}");
            n += 1;
        }
        out.insert(name, Value::Object(server));
    }
    out
}

fn operation_object(op: &Operation, action: Action, channel: &str, message: &str) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("action".into(), json!(action.as_str()));
    m.insert(
        "channel".into(),
        json!({ "$ref": format!("#/channels/{channel}") }),
    );
    if let Some(s) = &op.summary {
        m.insert("summary".into(), json!(s));
    }
    m.insert(
        "messages".into(),
        json!([{ "$ref": format!("#/channels/{channel}/messages/{message}") }]),
    );
    Value::Object(m)
}

/// The message this operation carries, and what to name it.
///
/// A handler that sends states its message as what it returns; one that
/// receives states it as what it accepts. Either side may be missing — a
/// consumer written as `fn on_event(Json<Event>) -> ()` has no return worth
/// reading — so each falls back to the other rather than emitting nothing.
fn message_for(op: &Operation, action: Action) -> (String, Value) {
    let shape = &op.shape;
    let produced = success_payload(shape);
    let consumed = shape.request.as_ref().map(|b| b.schema.clone());
    let payload = match action {
        Action::Send => produced.or(consumed),
        Action::Receive => consumed.or(produced),
    }
    .unwrap_or_else(|| json!({}));

    let name = component_name(&payload)
        .unwrap_or_else(|| format!("{}Message", capitalize(&op.operation_id)));
    (name, payload)
}

fn success_payload(shape: &RouteShape) -> Option<Value> {
    shape
        .responses
        .iter()
        .filter(|(code, _)| code.starts_with('2'))
        .min_by_key(|(code, _)| code.parse::<u16>().unwrap_or(u16::MAX))
        .map(|(_, r)| r.schema.clone())
        .filter(|s| !s.is_null())
}

/// Channel parameters for each `{placeholder}` in the address.
///
/// AsyncAPI parameters are not JSON Schemas — they carry a description and an
/// enumeration, nothing more — so a placeholder's Rust type does not survive
/// the crossing. The names do, and 3.0 requires them to be declared.
fn address_parameters(address: &str, shape: &RouteShape) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let mut rest = address;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        let name = rest[open + 1..open + close].to_string();
        rest = &rest[open + close + 1..];

        let mut param = serde_json::Map::new();
        if let Some(p) = shape.parameters.iter().find(|p| p.name == name) {
            if let Some(d) = p.schema["description"].as_str() {
                param.insert("description".into(), json!(d));
            }
            if let Some(values) = p.schema["enum"].as_array() {
                let strings: Vec<Value> = values
                    .iter()
                    .map(|v| json!(crate::compat::render(v)))
                    .collect();
                param.insert("enum".into(), Value::Array(strings));
            }
        }
        out.insert(name, Value::Object(param));
    }
    out
}

/// Query and header parameters describe an HTTP request, not a message.
fn note_unusable_parameters(op: &Operation, doc: &mut Document) {
    let mut dropped: Vec<&str> = op
        .shape
        .parameters
        .iter()
        .filter(|p| p.location != "path")
        .map(|p| p.name.as_str())
        .collect();
    if dropped.is_empty() {
        return;
    }
    dropped.sort_unstable();
    doc.notes.push(format!(
        "operation `{}`: {} parameter(s) ({}) have no place in a message-driven \
         API and were dropped — move them into the payload if consumers need them",
        op.operation_id,
        dropped.len(),
        dropped.join(", "),
    ));
}

/// The component a `$ref` names, if it is one.
fn component_name(schema: &Value) -> Option<String> {
    schema
        .get("$ref")?
        .as_str()?
        .strip_prefix("#/components/schemas/")
        .map(String::from)
}

/// A stable identifier for a channel, derived from its address.
///
/// Addresses are routinely paths (`item/events`) or templates
/// (`user/{id}/inbox`), and a channel identifier is a JSON Pointer segment —
/// so the address cannot be the key without escaping every reference to it.
fn channel_key(address: &str) -> String {
    let id = identifier(address);
    if id.is_empty() { "channel".into() } else { id }
}

/// `item/events` → `itemEvents`: lowerCamelCase over alphanumeric runs.
fn identifier(text: &str) -> String {
    let mut out = String::new();
    for word in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        if out.is_empty() {
            out.push_str(&word.to_ascii_lowercase());
        } else {
            out.push_str(&capitalize(word));
        }
    }
    out
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::route_sig::{Parameter, RequestBody, Response};

    fn info() -> Info {
        Info {
            title: "Events".into(),
            version: "1.0".into(),
            description: None,
            servers: vec!["kafka://broker.example.com:9092".into()],
        }
    }

    fn op(id: &str, method: &str, channel: &str, shape: RouteShape) -> Operation {
        Operation {
            operation_id: id.into(),
            method: method.into(),
            path: channel.into(),
            summary: None,
            shape,
        }
    }

    fn produces(schema: Value) -> RouteShape {
        let mut shape = RouteShape::default();
        shape.responses.insert("200".into(), Response::json(schema));
        shape
    }

    fn consumes(schema: Value) -> RouteShape {
        RouteShape {
            request: Some(RequestBody {
                content_type: "application/json".into(),
                schema,
                required: true,
            }),
            ..RouteShape::default()
        }
    }

    #[test]
    fn emits_a_three_zero_document_with_info_and_servers() {
        let d = document(&info(), &[]);
        assert_eq!(d.value["asyncapi"], "3.0.0");
        assert_eq!(d.value["info"]["title"], "Events");
        assert_eq!(
            d.value["servers"]["broker"]["host"],
            "broker.example.com:9092"
        );
        assert_eq!(d.value["servers"]["broker"]["protocol"], "kafka");
        assert!(d.conflicts.is_empty());
    }

    #[test]
    fn a_server_without_a_scheme_is_reported_rather_than_guessed_at() {
        let info = Info {
            servers: vec!["broker.example.com".into()],
            ..info()
        };
        let d = document(&info, &[]);
        assert!(d.value.get("servers").is_none());
        assert_eq!(d.notes.len(), 1, "got {:?}", d.notes);
        assert!(d.notes[0].contains("protocol"), "{:?}", d.notes);
    }

    #[test]
    fn subscribe_means_this_application_sends() {
        let d = document(
            &info(),
            &[op(
                "streamPrices",
                "SUBSCRIBE",
                "prices",
                produces(json!({ "$ref": "#/components/schemas/Price" })),
            )],
        );
        assert_eq!(d.value["operations"]["streamPrices"]["action"], "send");
    }

    #[test]
    fn publish_means_this_application_receives() {
        let d = document(
            &info(),
            &[op(
                "onOrder",
                "PUBLISH",
                "orders",
                consumes(json!({ "$ref": "#/components/schemas/Order" })),
            )],
        );
        assert_eq!(d.value["operations"]["onOrder"]["action"], "receive");
    }

    #[test]
    fn the_modern_vocabulary_says_the_same_thing_without_the_inversion() {
        let sent = document(&info(), &[op("a", "SEND", "c", produces(json!({})))]);
        let received = document(&info(), &[op("a", "RECEIVE", "c", consumes(json!({})))]);
        assert_eq!(sent.value["operations"]["a"]["action"], "send");
        assert_eq!(received.value["operations"]["a"]["action"], "receive");
    }

    #[test]
    fn a_channel_address_becomes_a_referenceable_identifier() {
        let d = document(
            &info(),
            &[op(
                "onSignup",
                "RECEIVE",
                "user/{id}/signup",
                consumes(json!({})),
            )],
        );
        let channel = &d.value["channels"]["userIdSignup"];
        assert_eq!(channel["address"], "user/{id}/signup");
        assert_eq!(
            d.value["operations"]["onSignup"]["channel"]["$ref"], "#/channels/userIdSignup",
            "the reference is a valid JSON Pointer segment"
        );
    }

    #[test]
    fn address_placeholders_are_declared_as_channel_parameters() {
        let mut shape = consumes(json!({}));
        shape.parameters.push(Parameter {
            name: "id".into(),
            location: "path".into(),
            required: true,
            schema: json!({ "type": "string", "description": "the user" }),
        });
        let d = document(
            &info(),
            &[op("onSignup", "RECEIVE", "user/{id}/signup", shape)],
        );
        let params = &d.value["channels"]["userIdSignup"]["parameters"];
        assert_eq!(params["id"]["description"], "the user");
    }

    #[test]
    fn query_parameters_are_dropped_with_a_note_not_silently() {
        let mut shape = consumes(json!({}));
        shape.parameters.push(Parameter {
            name: "verbose".into(),
            location: "query".into(),
            required: false,
            schema: json!({ "type": "boolean" }),
        });
        let d = document(&info(), &[op("onSignup", "RECEIVE", "signup", shape)]);
        assert_eq!(d.notes.len(), 1, "got {:?}", d.notes);
        assert!(d.notes[0].contains("verbose"), "{:?}", d.notes);
    }

    #[test]
    fn a_sender_takes_its_payload_from_what_the_handler_returns() {
        let mut shape = produces(json!({ "$ref": "#/components/schemas/Price" }));
        shape
            .components
            .insert("Price".into(), json!({ "type": "object" }));
        let d = document(&info(), &[op("streamPrices", "SEND", "prices", shape)]);
        assert_eq!(
            d.value["components"]["messages"]["Price"]["payload"]["$ref"],
            "#/components/schemas/Price"
        );
        assert_eq!(
            d.value["operations"]["streamPrices"]["messages"][0]["$ref"],
            "#/channels/prices/messages/Price"
        );
        assert_eq!(d.value["components"]["schemas"]["Price"]["type"], "object");
    }

    #[test]
    fn a_consumer_with_no_return_still_has_a_payload() {
        let d = document(
            &info(),
            &[op(
                "onOrder",
                "RECEIVE",
                "orders",
                consumes(json!({ "$ref": "#/components/schemas/Order" })),
            )],
        );
        assert_eq!(
            d.value["components"]["messages"]["Order"]["payload"]["$ref"],
            "#/components/schemas/Order"
        );
    }

    #[test]
    fn an_inline_payload_is_named_after_its_operation() {
        let d = document(
            &info(),
            &[op(
                "onPing",
                "RECEIVE",
                "ping",
                consumes(json!({ "type": "object" })),
            )],
        );
        assert_eq!(
            d.value["components"]["messages"]["OnPingMessage"]["payload"]["type"],
            "object"
        );
    }

    #[test]
    fn a_message_shared_by_two_operations_carries_neither_ones_summary() {
        // Messages are keyed by payload type, so a summary taken from one
        // handler's doc comment would be attached to the other's traffic too.
        let mut send = op(
            "publishOrder",
            "SEND",
            "orders",
            produces(json!({ "$ref": "#/components/schemas/Order" })),
        );
        send.summary = Some("Emits an order.".into());
        let mut receive = op(
            "onOrder",
            "RECEIVE",
            "orders",
            consumes(json!({ "$ref": "#/components/schemas/Order" })),
        );
        receive.summary = Some("Consumes an order.".into());

        let d = document(&info(), &[send, receive]);
        let message = &d.value["components"]["messages"]["Order"];
        assert!(message.get("summary").is_none(), "got {message:?}");
        // Each operation still says what it does.
        assert_eq!(
            d.value["operations"]["publishOrder"]["summary"],
            "Emits an order."
        );
        assert_eq!(
            d.value["operations"]["onOrder"]["summary"],
            "Consumes an order."
        );
    }

    #[test]
    fn two_operations_on_one_channel_share_it() {
        let d = document(
            &info(),
            &[
                op(
                    "send",
                    "SEND",
                    "chat",
                    produces(json!({ "$ref": "#/components/schemas/A" })),
                ),
                op(
                    "recv",
                    "RECEIVE",
                    "chat",
                    consumes(json!({ "$ref": "#/components/schemas/B" })),
                ),
            ],
        );
        let channel = &d.value["channels"]["chat"];
        assert!(channel["messages"]["A"].is_object());
        assert!(channel["messages"]["B"].is_object());
        assert_eq!(d.value["operations"].as_object().expect("ops").len(), 2);
        assert!(d.conflicts.is_empty(), "got {:?}", d.conflicts);
    }

    #[test]
    fn an_http_verb_is_a_conflict_not_a_guessed_action() {
        let d = document(
            &info(),
            &[op("getItem", "GET", "/items", RouteShape::default())],
        );
        assert_eq!(d.conflicts.len(), 1, "got {:?}", d.conflicts);
        assert!(
            d.value["operations"]
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
        );
    }

    #[test]
    fn a_duplicated_operation_id_is_reported() {
        let d = document(
            &info(),
            &[
                op("dup", "SEND", "a", produces(json!({}))),
                op("dup", "SEND", "b", produces(json!({}))),
            ],
        );
        assert_eq!(d.conflicts.len(), 1, "got {:?}", d.conflicts);
    }

    #[test]
    fn two_addresses_that_collapse_to_one_identifier_are_reported() {
        let d = document(
            &info(),
            &[
                op("a", "SEND", "item/events", produces(json!({}))),
                op("b", "SEND", "item.events", produces(json!({}))),
            ],
        );
        assert_eq!(d.conflicts.len(), 1, "got {:?}", d.conflicts);
        assert!(d.conflicts[0].contains("itemEvents"), "{:?}", d.conflicts);
    }
}
