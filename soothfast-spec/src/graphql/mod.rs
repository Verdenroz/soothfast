//! GraphQL SDL generated from inferred route shapes.
//!
//! Unlike the other dialects, this one cannot pass JSON Schema through: SDL
//! is a different type system, not a different serialization. Three
//! mismatches shape the whole module:
//!
//! - **Inputs and outputs are separate declarations.** A struct used as both
//!   a request body and a response becomes `Item` *and* `ItemInput`; GraphQL
//!   forbids one declaration serving both positions.
//! - **Nullability lives on the reference, not the object.** JSON Schema says
//!   `required: [id]` on the parent; SDL says `id: ID!` on the field.
//! - **Not everything crosses.** Untagged unions, flattened structs, maps and
//!   64-bit integers have no faithful SDL spelling. Each becomes a note and a
//!   `JSON` (or `Int64`) custom scalar rather than a quietly wrong type — the
//!   same "imprecise, never wrong" rule the extractor follows for gaps.
//!
//! The document this module builds is a type graph, not SDL text; [`to_sdl`]
//! renders it and [`diff`](diff::diff) compares two of them. Diffing the
//! graph rather than the text is what lets the gate say "field removed"
//! instead of "line 42 changed".

pub mod diff;
mod sdl;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::dialect::{Document, Info, Operation, unknown_method};

pub use sdl::{from_sdl, to_sdl};

/// The three root operation types, in the order SDL conventionally declares
/// them.
const ROOTS: &[(&str, &str)] = &[
    ("QUERY", "Query"),
    ("MUTATION", "Mutation"),
    ("SUBSCRIPTION", "Subscription"),
];

/// Whether a type is being reached as something a caller writes or as
/// something the server returns. GraphQL needs separate declarations for the
/// two, so the same component can arrive here twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    Input,
    Output,
}

/// Assemble a GraphQL type graph.
pub fn document(info: &Info, ops: &[Operation]) -> Document {
    let mut doc = Document::default();
    let components = merge_components(ops, &mut doc);
    let mut builder = Builder {
        components,
        types: BTreeMap::new(),
        notes: Vec::new(),
        stack: BTreeSet::new(),
    };

    let mut roots: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    for op in ops {
        let Some((_, root)) = ROOTS
            .iter()
            .find(|(m, _)| m.eq_ignore_ascii_case(&op.method))
        else {
            doc.conflicts.push(unknown_method(
                op,
                "GraphQL",
                "QUERY, MUTATION, SUBSCRIPTION",
            ));
            continue;
        };
        let fields = roots.entry((*root).to_string()).or_default();
        if fields.contains_key(&op.operation_id) {
            doc.conflicts.push(format!(
                "field `{}` is declared by more than one {root} operation",
                op.operation_id
            ));
            continue;
        }
        let field = builder.field(op);
        fields.insert(op.operation_id.clone(), field);
    }

    // Every GraphQL schema needs a query root, even one that only mutates.
    if !roots.is_empty() && !roots.contains_key("Query") {
        builder.notes.push(
            "no QUERY operations, but a GraphQL schema must declare a query \
             root — emitted with a placeholder field"
                .into(),
        );
        roots.insert(
            "Query".into(),
            [(
                "_empty".to_string(),
                json!({ "type": "Boolean",
                        "description": "Placeholder: this schema declares no queries." }),
            )]
            .into(),
        );
    }

    doc.notes.extend(builder.notes);
    doc.value = json!({
        "info": { "title": info.title, "version": info.version },
        "types": Value::Object(builder.types.into_iter().collect()),
        "roots": Value::Object(
            roots
                .into_iter()
                .map(|(k, v)| (k, Value::Object(v.into_iter().collect())))
                .collect(),
        ),
    });
    doc
}

/// One component map for the whole document, reporting definitions that
/// disagree rather than letting the last operation win.
fn merge_components(ops: &[Operation], doc: &mut Document) -> BTreeMap<String, Value> {
    let mut merged: BTreeMap<String, Value> = BTreeMap::new();
    for op in ops {
        for (name, schema) in &op.shape.components {
            match merged.get(name) {
                Some(existing) if existing != schema => doc.conflicts.push(format!(
                    "type `{name}` has two different definitions (second seen \
                     via operation `{}`)",
                    op.operation_id
                )),
                _ => {
                    merged.insert(name.clone(), schema.clone());
                }
            }
        }
    }
    merged
}

/// Walks JSON Schema, emitting GraphQL type declarations.
struct Builder {
    components: BTreeMap<String, Value>,
    types: BTreeMap<String, Value>,
    notes: Vec<String>,
    /// Declarations currently being built, so a self-referential type stops.
    stack: BTreeSet<String>,
}

impl Builder {
    /// One root field: its arguments, its type, and its description.
    fn field(&mut self, op: &Operation) -> Value {
        let mut args: BTreeMap<String, Value> = BTreeMap::new();
        for p in &op.shape.parameters {
            let Some(name) = self.valid_name(&p.name, &op.operation_id) else {
                continue;
            };
            let ty = self.type_ref(
                &p.schema,
                Position::Input,
                &format!("{}{}", capitalize(&op.operation_id), capitalize(&p.name)),
                p.required,
            );
            args.insert(name, json!({ "type": ty }));
        }
        if let Some(body) = &op.shape.request {
            // A body is one argument, not a splat of its fields: GraphQL
            // convention is `createItem(input: NewItemInput!)`, and expanding
            // it would collide with same-named parameters.
            let ty = self.type_ref(
                &body.schema,
                Position::Input,
                &format!("{}Input", capitalize(&op.operation_id)),
                body.required,
            );
            args.insert("input".into(), json!({ "type": ty }));
        }

        let ty = match success_schema(op) {
            Some(schema) => self.type_ref(
                &schema,
                Position::Output,
                &format!("{}Result", capitalize(&op.operation_id)),
                true,
            ),
            // A handler that returns nothing still has to return something in
            // GraphQL; `Boolean!` is the conventional "it happened".
            None => "Boolean!".into(),
        };

        let mut field = serde_json::Map::new();
        field.insert("type".into(), json!(ty));
        if let Some(s) = &op.summary {
            field.insert("description".into(), json!(s));
        }
        if !args.is_empty() {
            field.insert("args".into(), Value::Object(args.into_iter().collect()));
        }
        Value::Object(field)
    }

    /// A type reference, e.g. `Item`, `[String]` or `ID!`.
    ///
    /// `hint` names the declaration if this schema is an anonymous shape that
    /// has to become one; `required` decides the trailing `!`.
    fn type_ref(&mut self, schema: &Value, pos: Position, hint: &str, required: bool) -> String {
        let base = self.base_type(schema, pos, hint);
        if required { format!("{base}!") } else { base }
    }

    fn base_type(&mut self, schema: &Value, pos: Position, hint: &str) -> String {
        if let Some(name) = component_name(schema) {
            return self.declare_component(&name, pos);
        }

        // An enumeration of string constants is the one union GraphQL can
        // express faithfully.
        if schema.get("oneOf").is_some() || schema.get("enum").is_some() {
            return match self.enum_values(schema) {
                Some(values) => self.declare_enum(hint, values, schema),
                None => self.opaque(
                    hint,
                    "a union of shapes, which GraphQL can only express when \
                     every branch is a distinct object type and never in an \
                     input position",
                ),
            };
        }
        if schema.get("allOf").is_some() {
            return self.opaque(
                hint,
                "a composed (flattened or internally tagged) object, whose \
                 field set is not statically known here",
            );
        }

        match schema["type"].as_str() {
            Some("string") => "String".into(),
            Some("boolean") => "Boolean".into(),
            Some("number") => "Float".into(),
            Some("integer") => {
                // GraphQL's Int is 32-bit; calling a u64 an Int would silently
                // truncate every value above 2^31.
                if schema["format"].as_str() == Some("int64") {
                    self.scalar("Int64", "64-bit integer, serialized as a JSON number.");
                    "Int64".into()
                } else {
                    "Int".into()
                }
            }
            Some("array") => {
                let inner = self.base_type(&schema["items"], pos, &format!("{hint}Item"));
                // Items are left nullable: `Vec<Option<T>>` and `Vec<T>` reach
                // here identically, so `[T!]` would be a guarantee we cannot
                // make.
                format!("[{inner}]")
            }
            Some("object") => {
                if schema.get("properties").is_some() {
                    self.declare_object(hint, schema, pos)
                } else {
                    // A map: dynamic keys, which SDL has no way to declare.
                    self.opaque(hint, "a map with dynamic keys")
                }
            }
            Some("null") => self.opaque(hint, "a unit value, which SDL cannot name"),
            _ => self.opaque(hint, "an open schema (the extractor reported a gap here)"),
        }
    }

    /// Declare a named component under `pos`, returning the GraphQL type name.
    fn declare_component(&mut self, name: &str, pos: Position) -> String {
        let Some(schema) = self.components.get(name).cloned() else {
            return self.opaque(name, "a type with no definition in this crate");
        };
        // An enumeration is legal in both positions, so it keeps one name.
        if let Some(values) = self.enum_values(&schema) {
            return self.declare_enum(name, values, &schema);
        }
        if schema["type"].as_str() == Some("object") && schema.get("properties").is_some() {
            return self.declare_object(name, &schema, pos);
        }
        // A newtype over a scalar has no declaration of its own in SDL.
        self.base_type(&schema.clone(), pos, name)
    }

    /// Declare an object or input type, recursing into its fields.
    fn declare_object(&mut self, hint: &str, schema: &Value, pos: Position) -> String {
        let name = match pos {
            Position::Output => hint.to_string(),
            // Inputs are always suffixed, even when no output type shares the
            // name: a predictable name beats one that depends on what else
            // the schema happens to contain.
            Position::Input if hint.ends_with("Input") => hint.to_string(),
            Position::Input => format!("{hint}Input"),
        };
        if self.types.contains_key(&name) || self.stack.contains(&name) {
            return name;
        }
        self.stack.insert(name.clone());

        let required = crate::compat::required_set(schema);
        let mut fields: BTreeMap<String, Value> = BTreeMap::new();
        for (prop, prop_schema) in schema["properties"].as_object().into_iter().flatten() {
            let Some(field_name) = self.valid_name(prop, &name) else {
                continue;
            };
            let ty = self.type_ref(
                prop_schema,
                pos,
                &format!("{name}{}", capitalize(prop)),
                required.contains(prop.as_str()),
            );
            let mut field = serde_json::Map::new();
            field.insert("type".into(), json!(ty));
            if let Some(d) = prop_schema["description"].as_str() {
                field.insert("description".into(), json!(d));
            }
            fields.insert(field_name, Value::Object(field));
        }

        self.stack.remove(&name);
        let mut decl = serde_json::Map::new();
        decl.insert(
            "kind".into(),
            json!(match pos {
                Position::Input => "input",
                Position::Output => "type",
            }),
        );
        if let Some(d) = schema["description"].as_str() {
            decl.insert("description".into(), json!(d));
        }
        if fields.is_empty() {
            // SDL has no empty type body, and an object whose every field was
            // unrepresentable is not an object worth declaring.
            return self.opaque(hint, "an object with no field GraphQL can name");
        }
        decl.insert("fields".into(), Value::Object(fields.into_iter().collect()));
        self.types.insert(name.clone(), Value::Object(decl));
        name
    }

    /// The string constants an enumeration accepts, if that is what it is.
    ///
    /// Both spellings the extractor produces are read: a plain `enum` list,
    /// and the `oneOf` of `const` branches that serde's externally tagged
    /// unit variants become.
    fn enum_values(&self, schema: &Value) -> Option<Vec<String>> {
        if let Some(list) = schema["enum"].as_array() {
            return list
                .iter()
                .map(|v| v.as_str().map(String::from))
                .collect::<Option<Vec<_>>>()
                .filter(|v| !v.is_empty());
        }
        let branches = schema["oneOf"].as_array()?;
        if branches.is_empty() {
            return None;
        }
        branches
            .iter()
            .map(|b| b.get("const").and_then(|c| c.as_str()).map(String::from))
            .collect::<Option<Vec<_>>>()
    }

    fn declare_enum(&mut self, hint: &str, values: Vec<String>, schema: &Value) -> String {
        // GraphQL enum values are identifiers; a serde rename rule can produce
        // wire values (`kebab-case`, `with space`) that simply are not.
        let unusable: Vec<&String> = values.iter().filter(|v| !is_valid_name(v)).collect();
        if !unusable.is_empty() {
            let listed: Vec<&str> = unusable.iter().map(|s| s.as_str()).collect();
            self.notes.push(format!(
                "`{hint}`: enum value(s) {} are not valid GraphQL names, so the \
                 enumeration is emitted as `String` — renaming them in serde \
                 would let it cross",
                listed.join(", ")
            ));
            return "String".into();
        }
        if !self.types.contains_key(hint) {
            let mut decl = serde_json::Map::new();
            decl.insert("kind".into(), json!("enum"));
            if let Some(d) = schema["description"].as_str() {
                decl.insert("description".into(), json!(d));
            }
            decl.insert("values".into(), json!(values));
            self.types.insert(hint.to_string(), Value::Object(decl));
        }
        hint.to_string()
    }

    /// Fall back to a custom scalar, saying what could not be expressed.
    fn opaque(&mut self, at: &str, why: &str) -> String {
        self.scalar(
            "JSON",
            "Arbitrary JSON, for shapes the GraphQL type system cannot name.",
        );
        self.notes
            .push(format!("`{at}` is {why}; emitted as the `JSON` scalar"));
        "JSON".into()
    }

    fn scalar(&mut self, name: &str, description: &str) {
        self.types
            .entry(name.to_string())
            .or_insert_with(|| json!({ "kind": "scalar", "description": description }));
    }

    /// Accept a name SDL can spell, or report the one it cannot.
    fn valid_name(&mut self, name: &str, at: &str) -> Option<String> {
        if is_valid_name(name) {
            return Some(name.to_string());
        }
        self.notes.push(format!(
            "`{at}`: field `{name}` is not a valid GraphQL name and was \
             dropped — a `#[serde(rename)]` would let it cross"
        ));
        None
    }
}

/// The schema of the lowest 2xx response, if it carries a body.
fn success_schema(op: &Operation) -> Option<Value> {
    op.shape
        .responses
        .iter()
        .filter(|(code, _)| code.starts_with('2'))
        .min_by_key(|(code, _)| code.parse::<u16>().unwrap_or(u16::MAX))
        .map(|(_, r)| r.schema.clone())
        .filter(|s| !s.is_null() && s.as_object().is_some_and(|o| !o.is_empty()))
}

fn component_name(schema: &Value) -> Option<String> {
    schema
        .get("$ref")?
        .as_str()?
        .strip_prefix("#/components/schemas/")
        .map(String::from)
}

/// `/[_A-Za-z][_0-9A-Za-z]*/`, minus the `__` prefix GraphQL reserves for
/// introspection.
fn is_valid_name(name: &str) -> bool {
    if name.starts_with("__") {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
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
    use crate::schema::route_sig::{Parameter, RequestBody, Response, RouteShape};

    fn info() -> Info {
        Info {
            title: "Items".into(),
            version: "1.0".into(),
            description: None,
            servers: Vec::new(),
        }
    }

    fn op(id: &str, method: &str, shape: RouteShape) -> Operation {
        Operation {
            operation_id: id.into(),
            method: method.into(),
            path: id.into(),
            summary: None,
            shape,
        }
    }

    fn returns(schema: Value) -> RouteShape {
        let mut shape = RouteShape::default();
        shape.responses.insert("200".into(), Response::json(schema));
        shape
    }

    fn object(props: Value, required: &[&str]) -> Value {
        json!({ "type": "object", "properties": props, "required": required })
    }

    fn item_shape() -> RouteShape {
        let mut shape = returns(json!({ "$ref": "#/components/schemas/Item" }));
        shape.components.insert(
            "Item".into(),
            object(
                json!({ "id": { "type": "string" }, "note": { "type": "string" } }),
                &["id"],
            ),
        );
        shape
    }

    #[test]
    fn a_query_field_carries_its_return_type() {
        let d = document(&info(), &[op("item", "QUERY", item_shape())]);
        assert_eq!(d.value["roots"]["Query"]["item"]["type"], "Item!");
        assert_eq!(d.value["types"]["Item"]["kind"], "type");
        assert!(d.conflicts.is_empty(), "got {:?}", d.conflicts);
    }

    #[test]
    fn requiredness_becomes_field_nullability() {
        let d = document(&info(), &[op("item", "QUERY", item_shape())]);
        let fields = &d.value["types"]["Item"]["fields"];
        assert_eq!(fields["id"]["type"], "String!", "required is non-null");
        assert_eq!(fields["note"]["type"], "String", "optional is nullable");
    }

    #[test]
    fn parameters_become_field_arguments() {
        let mut shape = item_shape();
        shape.parameters.push(Parameter {
            name: "id".into(),
            location: "path".into(),
            required: true,
            schema: json!({ "type": "integer" }),
        });
        shape.parameters.push(Parameter {
            name: "verbose".into(),
            location: "query".into(),
            required: false,
            schema: json!({ "type": "boolean" }),
        });
        let d = document(&info(), &[op("item", "QUERY", shape)]);
        let args = &d.value["roots"]["Query"]["item"]["args"];
        assert_eq!(args["id"]["type"], "Int!");
        assert_eq!(args["verbose"]["type"], "Boolean");
    }

    #[test]
    fn a_request_body_becomes_one_input_argument() {
        let mut shape = returns(json!({ "$ref": "#/components/schemas/Item" }));
        shape.components.insert(
            "Item".into(),
            object(json!({ "id": { "type": "string" } }), &["id"]),
        );
        shape.components.insert(
            "NewItem".into(),
            object(json!({ "name": { "type": "string" } }), &["name"]),
        );
        shape.request = Some(RequestBody {
            content_type: "application/json".into(),
            schema: json!({ "$ref": "#/components/schemas/NewItem" }),
            required: true,
        });
        let d = document(&info(), &[op("createItem", "MUTATION", shape)]);
        let args = &d.value["roots"]["Mutation"]["createItem"]["args"];
        assert_eq!(args["input"]["type"], "NewItemInput!");
        assert_eq!(d.value["types"]["NewItemInput"]["kind"], "input");
    }

    #[test]
    fn one_struct_used_both_ways_becomes_two_declarations() {
        let mut shape = returns(json!({ "$ref": "#/components/schemas/Item" }));
        shape.components.insert(
            "Item".into(),
            object(json!({ "id": { "type": "string" } }), &["id"]),
        );
        shape.request = Some(RequestBody {
            content_type: "application/json".into(),
            schema: json!({ "$ref": "#/components/schemas/Item" }),
            required: true,
        });
        let d = document(&info(), &[op("echoItem", "MUTATION", shape)]);
        assert_eq!(d.value["types"]["Item"]["kind"], "type");
        assert_eq!(d.value["types"]["ItemInput"]["kind"], "input");
    }

    #[test]
    fn a_unit_variant_enum_crosses_as_a_graphql_enum() {
        let mut shape = returns(json!({ "$ref": "#/components/schemas/Status" }));
        shape.components.insert(
            "Status".into(),
            json!({ "oneOf": [ { "const": "ACTIVE" }, { "const": "ARCHIVED" } ] }),
        );
        let d = document(&info(), &[op("status", "QUERY", shape)]);
        assert_eq!(d.value["types"]["Status"]["kind"], "enum");
        assert_eq!(
            d.value["types"]["Status"]["values"],
            json!(["ACTIVE", "ARCHIVED"])
        );
        assert_eq!(d.value["roots"]["Query"]["status"]["type"], "Status!");
    }

    #[test]
    fn an_enum_whose_values_are_not_identifiers_falls_back_and_says_so() {
        let mut shape = returns(json!({ "$ref": "#/components/schemas/Status" }));
        shape.components.insert(
            "Status".into(),
            json!({ "oneOf": [ { "const": "in-progress" }, { "const": "done" } ] }),
        );
        let d = document(&info(), &[op("status", "QUERY", shape)]);
        assert_eq!(d.value["roots"]["Query"]["status"]["type"], "String!");
        assert!(
            d.notes.iter().any(|n| n.contains("in-progress")),
            "got {:?}",
            d.notes
        );
    }

    #[test]
    fn a_sixty_four_bit_integer_gets_its_own_scalar_rather_than_truncating() {
        let shape = returns(json!({ "type": "integer", "format": "int64" }));
        let d = document(&info(), &[op("count", "QUERY", shape)]);
        assert_eq!(d.value["roots"]["Query"]["count"]["type"], "Int64!");
        assert_eq!(d.value["types"]["Int64"]["kind"], "scalar");
    }

    #[test]
    fn a_map_becomes_the_json_scalar_with_a_note() {
        let shape =
            returns(json!({ "type": "object", "additionalProperties": { "type": "string" } }));
        let d = document(&info(), &[op("labels", "QUERY", shape)]);
        assert_eq!(d.value["roots"]["Query"]["labels"]["type"], "JSON!");
        assert!(
            d.notes.iter().any(|n| n.contains("dynamic keys")),
            "{:?}",
            d.notes
        );
    }

    #[test]
    fn a_union_of_objects_is_reported_rather_than_guessed() {
        let mut shape = returns(json!({ "$ref": "#/components/schemas/Event" }));
        shape.components.insert(
            "Event".into(),
            json!({ "oneOf": [
                { "type": "object", "properties": { "Created": { "type": "string" } } },
                { "type": "object", "properties": { "Deleted": { "type": "string" } } },
            ]}),
        );
        let d = document(&info(), &[op("event", "QUERY", shape)]);
        assert_eq!(d.value["roots"]["Query"]["event"]["type"], "JSON!");
        assert!(d.notes.iter().any(|n| n.contains("union")), "{:?}", d.notes);
    }

    #[test]
    fn an_array_leaves_its_items_nullable() {
        // `Vec<T>` and `Vec<Option<T>>` reach the emitter identically, so
        // `[T!]` would be a guarantee the code does not make.
        let shape = returns(json!({ "type": "array", "items": { "type": "string" } }));
        let d = document(&info(), &[op("tags", "QUERY", shape)]);
        assert_eq!(d.value["roots"]["Query"]["tags"]["type"], "[String]!");
    }

    #[test]
    fn a_handler_that_returns_nothing_still_returns_something() {
        let mut shape = RouteShape::default();
        shape.responses.insert("204".into(), Response::empty());
        let d = document(&info(), &[op("deleteItem", "MUTATION", shape)]);
        assert_eq!(
            d.value["roots"]["Mutation"]["deleteItem"]["type"],
            "Boolean!"
        );
    }

    #[test]
    fn a_schema_with_only_mutations_still_declares_a_query_root() {
        let d = document(&info(), &[op("createItem", "MUTATION", item_shape())]);
        assert!(d.value["roots"]["Query"]["_empty"].is_object());
        assert!(
            d.notes.iter().any(|n| n.contains("query root")),
            "{:?}",
            d.notes
        );
    }

    #[test]
    fn a_self_referential_type_terminates() {
        let mut shape = returns(json!({ "$ref": "#/components/schemas/Node" }));
        shape.components.insert(
            "Node".into(),
            object(
                json!({ "next": { "$ref": "#/components/schemas/Node" } }),
                &[],
            ),
        );
        let d = document(&info(), &[op("node", "QUERY", shape)]);
        assert_eq!(d.value["types"]["Node"]["fields"]["next"]["type"], "Node");
    }

    #[test]
    fn a_field_name_sdl_cannot_spell_is_dropped_with_a_note() {
        let mut shape = returns(json!({ "$ref": "#/components/schemas/Item" }));
        shape.components.insert(
            "Item".into(),
            object(
                json!({ "id": { "type": "string" }, "content-type": { "type": "string" } }),
                &["id"],
            ),
        );
        let d = document(&info(), &[op("item", "QUERY", shape)]);
        let fields = d.value["types"]["Item"]["fields"]
            .as_object()
            .expect("fields");
        assert!(fields.contains_key("id"));
        assert!(!fields.contains_key("content-type"));
        assert!(
            d.notes.iter().any(|n| n.contains("content-type")),
            "{:?}",
            d.notes
        );
    }

    #[test]
    fn an_http_verb_is_a_conflict_not_a_guessed_root() {
        let d = document(&info(), &[op("getItem", "GET", item_shape())]);
        assert_eq!(d.conflicts.len(), 1, "got {:?}", d.conflicts);
        assert!(d.value["roots"].as_object().is_some_and(|r| r.is_empty()));
    }

    #[test]
    fn two_fields_with_one_name_on_the_same_root_are_reported() {
        let d = document(
            &info(),
            &[
                op("item", "QUERY", item_shape()),
                op("item", "QUERY", item_shape()),
            ],
        );
        assert_eq!(d.conflicts.len(), 1, "got {:?}", d.conflicts);
    }

    #[test]
    fn subscriptions_land_on_their_own_root() {
        let d = document(&info(), &[op("itemAdded", "SUBSCRIPTION", item_shape())]);
        assert_eq!(
            d.value["roots"]["Subscription"]["itemAdded"]["type"],
            "Item!"
        );
    }
}
