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
use super::graphql_attrs;
use super::serde_attrs;
use super::types::{Resolver, Subst, generic_args};
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

        let mut schema = self.fields_object(Some(item), &ids, &names, stripped, subst, at);
        Self::describe(item, &mut schema);
        schema
    }

    /// Build an object schema from a list of struct-field ids.
    ///
    /// `container` is the struct's own item, when there is one to look a
    /// `#[ComplexObject]` impl up on — `None` for an enum variant's fields,
    /// which have no impl block of their own to search.
    fn fields_object(
        &mut self,
        container: Option<&'a Value>,
        ids: &[Value],
        names: &WireNames,
        stripped: bool,
        subst: &Subst,
        at: &str,
    ) -> Value {
        let mut fields = Vec::new();
        for id in ids {
            match self.resolve_field(id, names, subst, at) {
                Some(f) => fields.push(f),
                None => {
                    if let Some(container) = container
                        && let Some(f) = self.recover_complex_field(container, id, names, subst, at)
                    {
                        fields.push(f);
                    }
                }
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

    /// Recover a field a `#[graphql(skip)]` attribute took off the wire, from
    /// a matching `#[ComplexObject]` resolver method of the same name — the
    /// pattern async-graphql uses to compute a field (typically paginated)
    /// instead of storing it directly. `resolve_field` already returned
    /// `None` for this id for *some* reason; this only investigates further
    /// when that reason was specifically a graphql skip, so a field that is
    /// missing from the wire for any other reason is left alone.
    fn recover_complex_field(
        &mut self,
        container: &'a Value,
        id: &Value,
        names: &WireNames,
        subst: &Subst,
        at: &str,
    ) -> Option<Field> {
        let gql = names.graphql.as_ref()?;
        let item = self.item(id)?;
        let rust_name = item["name"].as_str()?.to_string();
        let empty = Vec::new();
        let raw = item["attrs"].as_array().unwrap_or(&empty);
        let f = graphql_attrs::field(raw);
        if !f.skip {
            return None;
        }
        let wire = graphql_attrs::wire_name_field(&rust_name, &f, gql);
        self.complex_object_field(container, &rust_name, wire, subst, at)
    }

    /// Find the `#[ComplexObject]`-generated inherent method matching
    /// `rust_name` on `container`, and resolve its return type as the
    /// field's schema.
    ///
    /// `#[ComplexObject]` is an attribute macro, not a derive: it rewrites
    /// the impl block entirely, so the user's method is only visible in
    /// rustdoc JSON as a separate function item at all when rustdoc ran with
    /// private items documented (`spec_gen`'s route discovery always does).
    /// Its inherent impl shows up in the struct's own `impls` array exactly
    /// like every other impl `serves_graphql` already scans — the search
    /// here is that same array, filtered for `trait: null` instead of a
    /// GraphQL container trait.
    fn complex_object_field(
        &mut self,
        container: &'a Value,
        rust_name: &str,
        wire: String,
        subst: &Subst,
        at: &str,
    ) -> Option<Field> {
        let impls = container["inner"]["struct"]["impls"].as_array()?;
        let method = impls
            .iter()
            .filter_map(|id| self.item(id))
            .filter(|im| im["inner"]["impl"]["trait"].is_null())
            .find_map(|im| {
                im["inner"]["impl"]["items"]
                    .as_array()?
                    .iter()
                    .find_map(|fid| {
                        let f = self.item(fid)?;
                        (f["name"].as_str() == Some(rust_name)
                            && f["inner"].get("function").is_some())
                        .then_some(f)
                    })
            })?;

        let where_ = format!("{at}.{wire}");
        let sig = &method["inner"]["function"]["sig"];
        let inputs = sig["inputs"].as_array().cloned().unwrap_or_default();
        // Input 0 is always `self`. The macro always injects a synthetic
        // `Context` parameter next, whichever the user's own signature
        // declared; every remaining input is a real GraphQL argument this
        // schema has no room for unless it defaults on its own (`Option`) —
        // a required one is reported rather than silently guessed at.
        for input in inputs.iter().skip(1) {
            let ty = &input[1];
            let inner_ty = ty.get("borrowed_ref").map(|b| &b["type"]).unwrap_or(ty);
            let is_context = inner_ty["resolved_path"]["path"]
                .as_str()
                .is_some_and(|p| p == "Context" || p.ends_with("::Context"));
            if is_context {
                continue;
            }
            if !Resolver::is_option(ty) {
                self.record(Gap::ComplexFieldArgument {
                    at: where_.clone(),
                    argument: input[0].as_str().unwrap_or("?").to_string(),
                });
                return None;
            }
        }

        let (schema, optional) = self.resolve_field_output(&sig["output"], subst, &where_);
        let mut schema = schema;
        Self::describe(method, &mut schema);
        Some(Field {
            wire,
            schema,
            required: !optional,
            flatten: false,
        })
    }

    /// A resolver method's return type, unwrapping `Result<T, E>` to its
    /// success arm — the error path fails the whole GraphQL response rather
    /// than shaping this one field, unlike a route handler's `Result`, which
    /// contributes a real error response and so is kept whole there. Returns
    /// the schema plus whether the field is optional (nullable), read off
    /// the *unwrapped* type so `Result<Option<T>>` is still detected right.
    fn resolve_field_output(&mut self, output: &Value, subst: &Subst, at: &str) -> (Value, bool) {
        if output["resolved_path"]["path"].as_str() == Some("Result") {
            let args = generic_args(&output["resolved_path"]);
            return match args.first() {
                Some(ok) => (self.resolve(ok, subst, at), Resolver::is_option(ok)),
                None => (json!({}), true),
            };
        }
        (self.resolve(output, subst, at), Resolver::is_option(output))
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
            // `None`: a variant's fields have no impl block of their own to
            // search for a `#[ComplexObject]` resolver.
            return Some(self.fields_object(None, &ids, names, stripped, subst, at));
        }
        None
    }
}
