//! The type graph, rendered as SDL text.
//!
//! Declarations emit grouped by kind and alphabetically within each group, so
//! regenerating an unchanged schema produces an identical file. Grouping is
//! by what a reader looks for — scalars and enums are vocabulary, inputs and
//! types are the shapes, roots are the entry points — rather than by the
//! order routes happened to link in.

use serde_json::Value;

/// Render a generated type graph as GraphQL SDL.
pub fn to_sdl(doc: &Value) -> String {
    let mut blocks: Vec<String> = Vec::new();

    let title = doc["info"]["title"].as_str().unwrap_or("");
    let version = doc["info"]["version"].as_str().unwrap_or("");
    if !title.is_empty() {
        blocks.push(format!("# {title} {version}").trim_end().to_string());
    }

    let types = doc["types"].as_object();
    for kind in ["scalar", "enum", "input", "type"] {
        for (name, decl) in types.into_iter().flatten() {
            if decl["kind"].as_str() != Some(kind) {
                continue;
            }
            blocks.push(declaration(name, decl));
        }
    }

    // Roots read last and in operational order, not alphabetically: a reader
    // opening the file wants the entry points after the vocabulary.
    for root in ["Query", "Mutation", "Subscription"] {
        let Some(fields) = doc["roots"][root].as_object() else {
            continue;
        };
        let mut lines = vec![format!("type {root} {{")];
        for (name, field) in fields {
            lines.extend(field_lines(name, field));
        }
        lines.push("}".into());
        blocks.push(lines.join("\n"));
    }

    if blocks.is_empty() {
        return String::new();
    }
    format!("{}\n", blocks.join("\n\n"))
}

/// Parse SDL previously rendered by [`to_sdl`] back into a type graph.
///
/// A deliberate inverse of `to_sdl`'s deterministic output, not a general
/// SDL parser: the spec gate reads committed generated schemas, and the
/// freshness check guarantees those are `to_sdl` output.
pub fn from_sdl(text: &str) -> Result<Value, String> {
    let mut info = serde_json::Map::new();
    let mut types = serde_json::Map::new();
    let mut roots = serde_json::Map::new();
    let mut description: Option<String> = None;
    let mut lines = text.lines();

    while let Some(line) = lines.next() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix("# ") {
            let (title, version) = match header.rsplit_once(' ') {
                Some((t, v)) => (t, v),
                None => (header, ""),
            };
            info.insert("title".into(), title.into());
            info.insert("version".into(), version.into());
        } else if line.starts_with(r#"""""#) {
            description = Some(read_description(line, &mut lines, "")?);
        } else if let Some(name) = line.strip_prefix("scalar ") {
            types.insert(
                name.into(),
                decl_value("scalar", description.take(), None, None),
            );
        } else if let Some(rest) = line.strip_prefix("enum ") {
            let name = block_name(rest, line)?;
            let mut values = Vec::new();
            for body in lines.by_ref() {
                let body = body.trim_end();
                if body == "}" {
                    break;
                }
                values.push(Value::String(body.trim_start().into()));
            }
            let values = Value::Array(values);
            types.insert(
                name.into(),
                decl_value("enum", description.take(), Some(values), None),
            );
        } else if let Some(rest) = line.strip_prefix("input ") {
            let name = block_name(rest, line)?;
            let fields = read_fields(&mut lines)?;
            types.insert(
                name.into(),
                decl_value("input", description.take(), None, Some(fields)),
            );
        } else if let Some(rest) = line.strip_prefix("type ") {
            let name = block_name(rest, line)?;
            let fields = read_fields(&mut lines)?;
            if matches!(name, "Query" | "Mutation" | "Subscription") {
                roots.insert(name.into(), Value::Object(fields));
            } else {
                types.insert(
                    name.into(),
                    decl_value("type", description.take(), None, Some(fields)),
                );
            }
        } else {
            return Err(format!("unrecognized SDL line {line:?}"));
        }
    }

    let mut doc = serde_json::Map::new();
    if !info.is_empty() {
        doc.insert("info".into(), Value::Object(info));
    }
    doc.insert("types".into(), Value::Object(types));
    doc.insert("roots".into(), Value::Object(roots));
    Ok(Value::Object(doc))
}

fn block_name<'a>(rest: &'a str, line: &str) -> Result<&'a str, String> {
    rest.strip_suffix(" {")
        .ok_or_else(|| format!("malformed SDL declaration {line:?}"))
}

fn decl_value(
    kind: &str,
    description: Option<String>,
    values: Option<Value>,
    fields: Option<serde_json::Map<String, Value>>,
) -> Value {
    let mut decl = serde_json::Map::new();
    decl.insert("kind".into(), kind.into());
    if let Some(d) = description {
        decl.insert("description".into(), d.into());
    }
    if let Some(v) = values {
        decl.insert("values".into(), v);
    }
    if let Some(f) = fields {
        decl.insert("fields".into(), Value::Object(f));
    }
    Value::Object(decl)
}

fn read_fields(lines: &mut std::str::Lines) -> Result<serde_json::Map<String, Value>, String> {
    let mut fields = serde_json::Map::new();
    let mut description: Option<String> = None;
    while let Some(line) = lines.next() {
        let line = line.trim_end();
        if line == "}" {
            return Ok(fields);
        }
        let body = line
            .strip_prefix("  ")
            .ok_or_else(|| format!("malformed SDL field line {line:?}"))?;
        if body.starts_with(r#"""""#) {
            description = Some(read_description(body, lines, "  ")?);
            continue;
        }
        // rsplit: argument lists contain ": " but GraphQL types cannot
        let (name_args, ty) = body
            .rsplit_once(": ")
            .ok_or_else(|| format!("malformed SDL field line {line:?}"))?;
        let mut field = serde_json::Map::new();
        if let Some(d) = description.take() {
            field.insert("description".into(), d.into());
        }
        field.insert("type".into(), ty.into());
        let name = match name_args.split_once('(') {
            Some((name, rest)) => {
                let inner = rest
                    .strip_suffix(')')
                    .ok_or_else(|| format!("malformed SDL arguments {line:?}"))?;
                let mut args = serde_json::Map::new();
                for arg in inner.split(", ") {
                    let (n, t) = arg
                        .split_once(": ")
                        .ok_or_else(|| format!("malformed SDL argument {arg:?}"))?;
                    args.insert(n.into(), serde_json::json!({ "type": t }));
                }
                field.insert("args".into(), Value::Object(args));
                name
            }
            None => name_args,
        };
        fields.insert(name.into(), Value::Object(field));
    }
    Err("unterminated SDL block".into())
}

fn read_description(
    first: &str,
    lines: &mut std::str::Lines,
    indent: &str,
) -> Result<String, String> {
    let delim = r#"""""#;
    let inline = first.strip_prefix(delim).unwrap_or(first);
    if let Some(body) = inline.strip_suffix(delim)
        && !inline.is_empty()
    {
        return Ok(body.replace(r#"\""""#, delim));
    }
    let mut body = Vec::new();
    for line in lines {
        let line = line.trim_end();
        if line == format!("{indent}{delim}") {
            return Ok(body.join("\n").replace(r#"\""""#, delim));
        }
        body.push(line.strip_prefix(indent).unwrap_or(line).to_string());
    }
    Err("unterminated SDL description".into())
}

fn declaration(name: &str, decl: &Value) -> String {
    let mut lines = Vec::new();
    lines.extend(description_lines(decl, ""));

    match decl["kind"].as_str() {
        Some("scalar") => {
            lines.push(format!("scalar {name}"));
            return lines.join("\n");
        }
        Some("enum") => {
            lines.push(format!("enum {name} {{"));
            for value in decl["values"].as_array().into_iter().flatten() {
                if let Some(v) = value.as_str() {
                    lines.push(format!("  {v}"));
                }
            }
            lines.push("}".into());
            return lines.join("\n");
        }
        _ => {}
    }

    let keyword = if decl["kind"] == "input" {
        "input"
    } else {
        "type"
    };
    lines.push(format!("{keyword} {name} {{"));
    for (field_name, field) in decl["fields"].as_object().into_iter().flatten() {
        lines.extend(field_lines(field_name, field));
    }
    lines.push("}".into());
    lines.join("\n")
}

/// One field, with its arguments and description, indented into a body.
fn field_lines(name: &str, field: &Value) -> Vec<String> {
    let mut lines = description_lines(field, "  ");
    let args = match field["args"].as_object() {
        Some(args) if !args.is_empty() => {
            let rendered: Vec<String> = args
                .iter()
                .map(|(n, a)| format!("{n}: {}", a["type"].as_str().unwrap_or("JSON")))
                .collect();
            format!("({})", rendered.join(", "))
        }
        _ => String::new(),
    };
    lines.push(format!(
        "  {name}{args}: {}",
        field["type"].as_str().unwrap_or("JSON")
    ));
    lines
}

/// A `"""…"""` description block, if the node carries one.
///
/// Descriptions come from doc comments, so they can contain anything —
/// including the block delimiter itself, which is escaped rather than
/// stripped so no prose is silently lost.
fn description_lines(node: &Value, indent: &str) -> Vec<String> {
    let Some(text) = node["description"].as_str().filter(|d| !d.is_empty()) else {
        return Vec::new();
    };
    let escaped = text.replace(r#"""""#, r#"\""""#);
    let body: Vec<&str> = escaped.lines().collect();
    if body.len() == 1 {
        // A one-line description with no delimiter risk reads better inline.
        return vec![format!(r#"{indent}"""{}""""#, body[0].trim())];
    }
    let mut lines = vec![format!(r#"{indent}""""#)];
    lines.extend(body.iter().map(|l| format!("{indent}{}", l.trim_end())));
    lines.push(format!(r#"{indent}""""#));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc() -> Value {
        json!({
            "info": { "title": "Items", "version": "1.0" },
            "types": {
                "Item": { "kind": "type", "description": "One item.",
                          "fields": { "id": { "type": "String!" },
                                      "tags": { "type": "[String]" } } },
                "NewItemInput": { "kind": "input",
                                  "fields": { "name": { "type": "String!" } } },
                "Status": { "kind": "enum", "values": ["ACTIVE", "ARCHIVED"] },
                "JSON": { "kind": "scalar", "description": "Arbitrary JSON." },
            },
            "roots": {
                "Query": { "item": { "type": "Item!", "description": "Fetch one.",
                                     "args": { "id": { "type": "Int!" } } } },
                "Mutation": { "createItem": { "type": "Item!",
                                              "args": { "input": { "type": "NewItemInput!" } } } },
            },
        })
    }

    #[test]
    fn renders_every_kind_in_reading_order() {
        let sdl = to_sdl(&doc());
        let at = |needle: &str| {
            sdl.find(needle)
                .unwrap_or_else(|| panic!("{needle}\n{sdl}"))
        };
        assert!(at("scalar JSON") < at("enum Status"), "{sdl}");
        assert!(at("enum Status") < at("input NewItemInput"), "{sdl}");
        assert!(at("input NewItemInput") < at("type Item"), "{sdl}");
        assert!(at("type Item") < at("type Query"), "{sdl}");
        assert!(at("type Query") < at("type Mutation"), "{sdl}");
    }

    #[test]
    fn fields_carry_their_arguments_and_types() {
        let sdl = to_sdl(&doc());
        assert!(sdl.contains("  item(id: Int!): Item!"), "{sdl}");
        assert!(
            sdl.contains("  createItem(input: NewItemInput!): Item!"),
            "{sdl}"
        );
        assert!(sdl.contains("  tags: [String]"), "{sdl}");
    }

    #[test]
    fn descriptions_render_as_sdl_blocks() {
        let sdl = to_sdl(&doc());
        assert!(sdl.contains("\"\"\"One item.\"\"\""), "{sdl}");
        assert!(
            sdl.contains("  \"\"\"Fetch one.\"\"\""),
            "field-level:\n{sdl}"
        );
    }

    #[test]
    fn a_description_containing_the_delimiter_is_escaped_not_dropped() {
        let d = json!({
            "info": {},
            "types": { "X": { "kind": "scalar", "description": "a \"\"\" b" } },
            "roots": {},
        });
        let sdl = to_sdl(&d);
        assert!(sdl.contains(r#"\""""#), "{sdl}");
        assert!(sdl.contains("a "), "the prose survives:\n{sdl}");
    }

    #[test]
    fn a_multiline_description_opens_and_closes_on_its_own_lines() {
        let d = json!({
            "info": {},
            "types": { "X": { "kind": "type", "description": "One.\nTwo.",
                              "fields": { "a": { "type": "Int" } } } },
            "roots": {},
        });
        let sdl = to_sdl(&d);
        assert!(
            sdl.starts_with("\"\"\"\nOne.\nTwo.\n\"\"\"\ntype X {"),
            "{sdl}"
        );
    }

    #[test]
    fn the_header_names_the_schema() {
        assert!(to_sdl(&doc()).starts_with("# Items 1.0\n"));
    }

    #[test]
    fn rendering_is_byte_identical_between_runs() {
        assert_eq!(to_sdl(&doc()), to_sdl(&doc()));
        assert!(to_sdl(&doc()).ends_with('\n'));
    }

    #[test]
    fn an_empty_graph_renders_to_nothing() {
        assert_eq!(to_sdl(&json!({ "types": {}, "roots": {} })), "");
    }

    #[test]
    fn from_sdl_inverts_to_sdl() {
        let rendered = to_sdl(&doc());
        let parsed = from_sdl(&rendered).expect("parses");
        assert_eq!(to_sdl(&parsed), rendered);
        assert_eq!(
            parsed["types"]["Status"]["values"],
            json!(["ACTIVE", "ARCHIVED"])
        );
        assert_eq!(
            parsed["roots"]["Query"]["item"]["args"]["id"]["type"],
            "Int!"
        );
        assert_eq!(
            parsed["types"]["Item"]["fields"]["tags"]["type"],
            "[String]"
        );
    }

    #[test]
    fn from_sdl_round_trips_descriptions_and_escapes() {
        let d = json!({
            "info": {},
            "types": { "X": { "kind": "type", "description": "One.\nTwo.",
                              "fields": { "a": { "type": "Int",
                                                 "description": "a \"\"\" b" } } } },
            "roots": {},
        });
        let rendered = to_sdl(&d);
        let parsed = from_sdl(&rendered).expect("parses");
        assert_eq!(to_sdl(&parsed), rendered);
        assert_eq!(parsed["types"]["X"]["description"], "One.\nTwo.");
        assert_eq!(
            parsed["types"]["X"]["fields"]["a"]["description"],
            "a \"\"\" b"
        );
    }

    #[test]
    fn from_sdl_rejects_sdl_this_module_did_not_render() {
        assert!(from_sdl("interface Node { id: ID! }").is_err());
    }

    #[test]
    fn from_sdl_of_empty_text_is_the_empty_graph() {
        assert_eq!(
            from_sdl("").expect("parses"),
            json!({ "types": {}, "roots": {} })
        );
    }
}
