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
}
