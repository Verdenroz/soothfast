//! Struct and enum bodies, rendered under serde's wire representations.
//!
//! The representation rules mirror serde's own: externally tagged by default,
//! internally tagged with `tag`, adjacently tagged with `tag` + `content`,
//! and flat under `untagged`. Getting these wrong produces a schema that is
//! confidently incorrect rather than merely imprecise, so each shape is
//! rendered explicitly rather than approximated as a generic object.
//!
//! Naming is a separate question from representation, and not always serde's
//! — see [`wire_names`](super::wire_names).

use serde_json::{Value, json};

use super::Gap;
use super::serde_attrs;
use super::types::{Resolver, Subst};
use super::wire_names::WireNames;

/// One resolved field, ready to place into an object schema.
struct Field {
    wire: String,
    schema: Value,
    required: bool,
    flatten: bool,
}

impl<'a> Resolver<'a> {
    /// A struct body: unit, newtype, tuple, or plain named fields.
    pub(super) fn walk_struct(
        &mut self,
        item: &'a Value,
        s: &'a Value,
        subst: &Subst,
        at: &str,
    ) -> Value {
        let names = self.container_attrs(item, at);
        let kind = &s["kind"];

        if kind.as_str() == Some("unit") {
            return json!({ "type": "null" });
        }
        if let Some(t) = kind.get("tuple").and_then(|t| t.as_array()) {
            let types: Vec<Value> = t
                .iter()
                .filter_map(|id| self.item(id))
                .map(|f| f["inner"]["struct_field"].clone())
                .collect();
            // A newtype is transparent on the wire; wider tuples are arrays.
            if types.len() == 1 {
                let mut schema = self.resolve(&types[0], subst, at);
                Self::describe(item, &mut schema);
                return schema;
            }
            let items: Vec<Value> = types.iter().map(|ty| self.resolve(ty, subst, at)).collect();
            let n = items.len();
            let mut schema =
                json!({ "type": "array", "prefixItems": items, "minItems": n, "maxItems": n });
            Self::describe(item, &mut schema);
            return schema;
        }

        let Some(plain) = kind.get("plain") else {
            return json!({});
        };
        let ids: Vec<Value> = plain["fields"]
            .as_array()
            .map(|a| a.to_vec())
            .unwrap_or_default();
        let stripped = plain["has_stripped_fields"].as_bool().unwrap_or(false);

        // `transparent` forwards the wire shape of the single visible field.
        if names.serde.transparent
            && let Some(ty) = ids
                .first()
                .and_then(|id| self.item(id))
                .map(|f| f["inner"]["struct_field"].clone())
        {
            return self.resolve(&ty, subst, at);
        }

        let mut schema = self.fields_object(&ids, &names, stripped, subst, at);
        Self::describe(item, &mut schema);
        schema
    }

    /// Build an object schema from a list of struct-field ids.
    fn fields_object(
        &mut self,
        ids: &[Value],
        names: &WireNames,
        stripped: bool,
        subst: &Subst,
        at: &str,
    ) -> Value {
        let mut fields = Vec::new();
        for id in ids {
            if let Some(f) = self.resolve_field(id, names, subst, at) {
                fields.push(f);
            }
        }

        let mut properties = serde_json::Map::new();
        let mut required: Vec<String> = Vec::new();
        let mut merged: Vec<Value> = Vec::new();

        for f in fields {
            if f.flatten {
                // A flattened field contributes its own properties here, and
                // makes the object open when its shape is not an object.
                merged.push(f.schema);
                continue;
            }
            if f.required {
                required.push(f.wire.clone());
            }
            properties.insert(f.wire, f.schema);
        }

        let mut schema = json!({ "type": "object", "properties": properties });
        if !required.is_empty() {
            schema["required"] = json!(required);
        }
        // Private fields serde still emits, or a flattened non-object, mean
        // the property list is knowingly incomplete.
        if stripped {
            self.record(Gap::StrippedFields { at: at.to_string() });
        }
        if stripped || !merged.is_empty() {
            schema["additionalProperties"] = json!(true);
        }
        if !merged.is_empty() {
            let mut all = vec![schema];
            all.extend(merged);
            return json!({ "allOf": all });
        }
        if names.serde.deny_unknown_fields {
            schema["additionalProperties"] = json!(false);
        }
        schema
    }

    fn resolve_field(
        &mut self,
        id: &Value,
        names: &WireNames,
        subst: &Subst,
        at: &str,
    ) -> Option<Field> {
        let item = self.item(id)?;
        let name = item["name"].as_str()?.to_string();
        let empty = Vec::new();
        let raw = item["attrs"].as_array().unwrap_or(&empty);
        let attrs = serde_attrs::field(raw);
        let wire = names.field(&name, raw, &attrs)?;
        let where_ = format!("{at}.{wire}");
        let ty = &item["inner"]["struct_field"];

        // A custom serializer replaces the type's wire shape with whatever
        // the named code emits, so the type stops being evidence.
        if let Some(with) = &attrs.custom_serializer {
            self.record(Gap::CustomSerializer {
                at: where_.clone(),
                with: with.clone(),
            });
            return Some(Field {
                wire,
                schema: json!({}),
                required: !attrs.default,
                flatten: attrs.flatten,
            });
        }

        let optional = Resolver::is_option(ty) || attrs.default;
        let mut schema = self.resolve(ty, subst, &where_);
        Self::describe(item, &mut schema);
        Some(Field {
            wire,
            schema,
            required: !optional,
            flatten: attrs.flatten,
        })
    }

    /// An enum body under whichever serde tagging the container declares.
    pub(super) fn walk_enum(
        &mut self,
        item: &'a Value,
        e: &'a Value,
        subst: &Subst,
        at: &str,
    ) -> Value {
        let names = self.container_attrs(item, at);
        let ids: Vec<Value> = e["variants"]
            .as_array()
            .map(|a| a.to_vec())
            .unwrap_or_default();

        // A C-like enum has no per-variant shape to distinguish, so the
        // tag-free representations collapse to the one scalar schema.
        if names.serde.tag.is_none()
            && let Some(wire) = self.unit_variant_names(&ids, &names)
        {
            let mut schema = if names.serde.untagged {
                // Each untagged unit variant serializes as `null`, so a
                // `oneOf` of nulls is a branch set nothing can satisfy.
                json!({ "type": "null" })
            } else {
                json!({ "type": "string", "enum": wire })
            };
            Self::describe(item, &mut schema);
            return schema;
        }

        let mut branches = Vec::new();
        for id in &ids {
            if let Some(b) = self.variant_branch(id, &names, subst, at) {
                branches.push(b);
            }
        }
        if branches.is_empty() {
            return json!({});
        }
        let mut schema = json!({ "oneOf": branches });
        Self::describe(item, &mut schema);
        schema
    }

    /// Wire names of every variant in declaration order, or `None` unless the
    /// enum is non-empty and every variant is a unit variant.
    fn unit_variant_names(&self, ids: &[Value], names: &WireNames) -> Option<Vec<String>> {
        if ids.is_empty() {
            return None;
        }
        let mut wire = Vec::with_capacity(ids.len());
        for id in ids {
            let item = self.item(id)?;
            // Only a fieldless `V` is a bare string; `V()` and `V {}` still
            // go on the wire wrapped in their externally tagged object.
            if item["inner"]["variant"]["kind"].as_str() != Some("plain") {
                return None;
            }
            let name = item["name"].as_str()?;
            let empty = Vec::new();
            let raw = item["attrs"].as_array().unwrap_or(&empty);
            wire.push(names.variant(name, raw));
        }
        Some(wire)
    }

    fn variant_branch(
        &mut self,
        id: &Value,
        names: &WireNames,
        subst: &Subst,
        at: &str,
    ) -> Option<Value> {
        let item = self.item(id)?;
        let name = item["name"].as_str()?.to_string();
        let empty = Vec::new();
        let wire = names.variant(&name, item["attrs"].as_array().unwrap_or(&empty));
        let where_ = format!("{at}::{wire}");
        let kind = &item["inner"]["variant"]["kind"];

        let payload = self.variant_payload(kind, names, subst, &where_);

        Some(
            match (&names.serde.tag, &names.serde.content, names.serde.untagged) {
                // Untagged: the payload alone, with unit variants as null.
                (_, _, true) => payload.unwrap_or(json!({ "type": "null" })),

                // Adjacently tagged: tag beside the payload under `content`.
                (Some(tag), Some(content), _) => {
                    let mut props = serde_json::Map::new();
                    props.insert(tag.clone(), json!({ "const": wire }));
                    let mut required = vec![tag.clone()];
                    if let Some(p) = payload {
                        props.insert(content.clone(), p);
                        required.push(content.clone());
                    }
                    json!({ "type": "object", "properties": props, "required": required })
                }

                // Internally tagged: tag merged into the payload object.
                (Some(tag), None, _) => {
                    let tag_only = json!({ "type": "object",
                                       "properties": { tag.clone(): { "const": wire } },
                                       "required": [tag.clone()] });
                    match payload {
                        None => tag_only,
                        // An inline object can absorb the tag directly.
                        Some(mut schema) if schema.get("properties").is_some() => {
                            schema["properties"][tag.clone()] = json!({ "const": wire });
                            let mut required: Vec<String> = schema["required"]
                                .as_array()
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|v| v.as_str().map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default();
                            required.insert(0, tag.clone());
                            schema["required"] = json!(required);
                            schema
                        }
                        // A newtype variant wrapping a struct resolves to a $ref,
                        // which has no properties to merge into — compose instead
                        // of dropping the payload.
                        Some(schema) => json!({ "allOf": [tag_only, schema] }),
                    }
                }

                // Externally tagged (serde's default).
                (None, _, _) => match payload {
                    None => json!({ "const": wire }),
                    Some(p) => json!({ "type": "object",
                                   "properties": { wire.clone(): p },
                                   "required": [wire],
                                   "additionalProperties": false }),
                },
            },
        )
    }

    /// The payload schema of one variant, or `None` for a unit variant.
    fn variant_payload(
        &mut self,
        kind: &Value,
        names: &WireNames,
        subst: &Subst,
        at: &str,
    ) -> Option<Value> {
        if kind.as_str() == Some("plain") {
            return None;
        }
        if let Some(t) = kind.get("tuple").and_then(|t| t.as_array()) {
            let types: Vec<Value> = t
                .iter()
                .filter_map(|id| self.item(id))
                .map(|f| f["inner"]["struct_field"].clone())
                .collect();
            if types.is_empty() {
                return None;
            }
            if types.len() == 1 {
                return Some(self.resolve(&types[0], subst, at));
            }
            let items: Vec<Value> = types.iter().map(|ty| self.resolve(ty, subst, at)).collect();
            let n = items.len();
            return Some(
                json!({ "type": "array", "prefixItems": items, "minItems": n, "maxItems": n }),
            );
        }
        if let Some(st) = kind.get("struct") {
            let ids: Vec<Value> = st["fields"]
                .as_array()
                .map(|a| a.to_vec())
                .unwrap_or_default();
            let stripped = st["has_stripped_fields"].as_bool().unwrap_or(false);
            // Variant fields follow the container's rename rule, as in serde.
            return Some(self.fields_object(&ids, names, stripped, subst, at));
        }
        None
    }
}
