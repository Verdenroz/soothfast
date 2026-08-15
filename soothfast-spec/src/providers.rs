//! Declared-surface providers: parse each spec dialect into [`DeclaredOp`]s.
//! YAML converts to `serde_json::Value` so one walker serves YAML and JSON;
//! GraphQL SDL uses a hand-rolled field extractor (we need field names, not
//! an AST — dependency policy).

use serde_json::Value;

use crate::{DeclaredOp, SpecKind, serialize};

/// Parse one spec document into the operations it declares, dispatching on
/// dialect: OpenAPI, AsyncAPI, and MCP tool schemas are YAML-or-JSON;
/// GraphQL is SDL text.
pub fn parse(kind: SpecKind, text: &str) -> Result<Vec<DeclaredOp>, String> {
    match kind {
        SpecKind::OpenApi => openapi(&serialize::from_text(text)?),
        SpecKind::AsyncApi => asyncapi(&serialize::from_text(text)?),
        SpecKind::McpTools => mcp_tools(&serialize::from_text(text)?),
        SpecKind::GraphQl => graphql(text),
    }
}

const HTTP_METHODS: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

fn openapi(doc: &Value) -> Result<Vec<DeclaredOp>, String> {
    let paths = doc["paths"]
        .as_object()
        .ok_or("OpenAPI spec has no `paths` object")?;
    let mut ops = Vec::new();
    for (path, methods) in paths {
        let Some(methods) = methods.as_object() else {
            continue;
        };
        for (method, op) in methods {
            if !HTTP_METHODS.contains(&method.as_str()) {
                continue;
            }
            let operation = op["operationId"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("{} {path}", method.to_uppercase()));
            ops.push(DeclaredOp {
                operation,
                method: method.to_uppercase(),
                path: path.clone(),
            });
        }
    }
    Ok(ops)
}

/// AsyncAPI, in either of its two incompatible layouts.
///
/// 2.x hangs `publish`/`subscribe` off the channel; 3.0 moved them to a
/// top-level `operations` map keyed by operation id, with `action: send` or
/// `receive` and a `$ref` back to the channel. Both are still in the wild —
/// generated files are 3.0, hand-authored ones are frequently 2.x — so the
/// reader takes whichever the document actually uses.
fn asyncapi(doc: &Value) -> Result<Vec<DeclaredOp>, String> {
    if let Some(operations) = doc["operations"].as_object() {
        return Ok(asyncapi_v3(doc, operations));
    }
    let channels = doc["channels"]
        .as_object()
        .ok_or("AsyncAPI spec has no `channels` or `operations` object")?;
    let mut ops = Vec::new();
    for (channel, body) in channels {
        for verb in ["subscribe", "publish"] {
            let op = &body[verb];
            if op.is_null() {
                continue;
            }
            let operation = op["operationId"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("{channel}.{verb}"));
            ops.push(DeclaredOp {
                operation,
                method: verb.to_uppercase(),
                path: channel.clone(),
            });
        }
    }
    Ok(ops)
}

/// 3.0 operations, reported in 2.x's vocabulary.
///
/// `send`/`receive` are written from the application's point of view and
/// `publish`/`subscribe` from the client's, so they are opposites: an
/// operation the application *sends* is one a client *subscribes* to.
/// Reporting the 2.x spelling keeps one `#[route(method = ...)]` annotation
/// matching the same surface whichever version the file is written in.
fn asyncapi_v3(doc: &Value, operations: &serde_json::Map<String, Value>) -> Vec<DeclaredOp> {
    operations
        .iter()
        .map(|(id, op)| {
            let method = match op["action"].as_str() {
                Some("send") => "SUBSCRIBE",
                Some("receive") => "PUBLISH",
                _ => "SEND",
            };
            DeclaredOp {
                operation: id.clone(),
                method: method.to_string(),
                path: channel_address(doc, &op["channel"]),
            }
        })
        .collect()
}

/// The address of the channel an operation points at, following the one
/// `$ref` hop a 3.0 operation always takes to reach it.
fn channel_address(doc: &Value, channel: &Value) -> String {
    let resolved = match channel["$ref"].as_str() {
        Some(pointer) => {
            let mut node = doc;
            for token in pointer.trim_start_matches('#').split('/').skip(1) {
                if token.is_empty() {
                    continue;
                }
                node = &node[token.replace("~1", "/").replace("~0", "~")];
            }
            node
        }
        None => channel,
    };
    resolved["address"]
        .as_str()
        .or_else(|| channel["$ref"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn mcp_tools(doc: &Value) -> Result<Vec<DeclaredOp>, String> {
    let tools = doc["tools"]
        .as_array()
        .ok_or("MCP tool spec has no `tools` array")?;
    Ok(tools
        .iter()
        .filter_map(|t| t["name"].as_str())
        .map(|name| DeclaredOp {
            operation: name.to_string(),
            method: "TOOL".into(),
            path: name.to_string(),
        })
        .collect())
}

/// Field names of the root operation types. Line-oriented: enough for
/// reconciliation, deliberately not a GraphQL parser.
fn graphql(text: &str) -> Result<Vec<DeclaredOp>, String> {
    let (query_ty, mutation_ty, subscription_ty) = root_type_names(text);
    let type_tags = [
        (format!("type {query_ty}"), "QUERY"),
        (format!("type {mutation_ty}"), "MUTATION"),
        (format!("type {subscription_ty}"), "SUBSCRIPTION"),
    ];

    let mut ops = Vec::new();
    let mut current: Option<&str> = None; // QUERY / MUTATION / SUBSCRIPTION
    let mut depth = 0i32;
    // Field arguments sit at the same brace depth as the field itself, often
    // spanning lines — track parens too so a field is only extracted when a
    // line starts outside any still-open argument list.
    let mut paren_depth = 0i32;
    let mut in_description = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let Some(line) = strip_description(line, &mut in_description) else {
            continue;
        };
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if current.is_none() {
            for (ty, tag) in &type_tags {
                if line.starts_with(ty.as_str())
                    && line[ty.len()..].trim_start().starts_with(['{', '@'])
                        | line[ty.len()..].is_empty()
                        | line[ty.len()..].starts_with(' ')
                {
                    current = Some(tag);
                    depth = 0;
                    paren_depth = 0;
                }
            }
        }
        if let Some(tag) = current {
            let starts_fresh = paren_depth == 0;
            depth += line.matches('{').count() as i32;
            depth -= line.matches('}').count() as i32;
            paren_depth += line.matches('(').count() as i32;
            paren_depth -= line.matches(')').count() as i32;
            // A field line directly inside the type block: `name(args): Type`
            // or `name: Type` — not a continuation of an open argument list.
            if depth == 1 && starts_fresh && !line.contains("type ") {
                let name: String = line
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() && (line.contains(':') || line.contains('(')) {
                    ops.push(DeclaredOp {
                        operation: name.clone(),
                        method: tag.into(),
                        path: name,
                    });
                }
            }
            if depth <= 0 && line.contains('}') {
                current = None;
            }
        }
    }
    if ops.is_empty() {
        return Err("no Query/Mutation/Subscription fields found in SDL".into());
    }
    Ok(ops)
}

/// Root operation type names, from an explicit `schema { query: X
/// mutation: Y subscription: Z }` definition when present. GraphQL only
/// requires the canonical `Query`/`Mutation`/`Subscription` names when
/// there's no explicit schema block (typical hand-written SDL) — generated
/// schemas commonly use other names for their root types (e.g.
/// async-graphql's merged-object pattern produces `QueryRoot`), and an
/// explicit `schema { }` block is exactly how SDL says so.
fn root_type_names(text: &str) -> (String, String, String) {
    let mut query = "Query".to_string();
    let mut mutation = "Mutation".to_string();
    let mut subscription = "Subscription".to_string();
    let mut in_schema = false;
    let mut in_description = false;
    let mut depth = 0i32;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let Some(line) = strip_description(line, &mut in_description) else {
            continue;
        };
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if !in_schema {
            // `schema`, never part of a longer identifier (`schemaVersion`)
            // or a field (`schema: String`) — optionally followed by a
            // directive (`schema @link(...) {`, common in Apollo Federation
            // subgraphs) before the opening brace, on this line or the next.
            if line == "schema"
                || line.starts_with("schema ")
                || line.starts_with("schema@")
                || line.starts_with("schema{")
            {
                in_schema = true;
                depth = 0;
            } else {
                continue;
            }
        }
        depth += line.matches('{').count() as i32;
        depth -= line.matches('}').count() as i32;
        // Scan for each root-type keyword independently rather than
        // splitting the whole line on its first `:` — a compact single-line
        // block (`schema { query: Q, mutation: M }`) has more than one
        // `key: Value` pair, and a naive whole-line split only ever finds
        // the first.
        for (keyword, slot) in [
            ("query", &mut query),
            ("mutation", &mut mutation),
            ("subscription", &mut subscription),
        ] {
            if let Some(value) = find_root_type_value(line, keyword) {
                *slot = value;
            }
        }
        if depth <= 0 && line.contains('}') {
            break;
        }
    }
    (query, mutation, subscription)
}

/// Find a whole-word `keyword` in `line` followed (after optional
/// whitespace) by `:`, and return the identifier that follows it — the
/// value of one `key: Value` pair in a `schema { }` block.
fn find_root_type_value(line: &str, keyword: &str) -> Option<String> {
    let bytes = line.as_bytes();
    let is_word_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = 0;
    while let Some(rel) = line[start..].find(keyword) {
        let idx = start + rel;
        let before_ok = idx == 0 || !is_word_byte(bytes[idx - 1]);
        let after = idx + keyword.len();
        let after_ok = after >= bytes.len() || !is_word_byte(bytes[after]);
        if before_ok && after_ok {
            let rest = line[after..].trim_start();
            if let Some(rest) = rest.strip_prefix(':') {
                let value: String = rest
                    .trim_start()
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
        start = idx + keyword.len();
    }
    None
}

/// Strips a `"""..."""` description down to whatever real SDL (if any)
/// follows its closing marker on the same line — a closing `"""` sharing a
/// line with the next field/keyword is common formatter output and must not
/// silently drop that content. `None` means the whole line was consumed by
/// (or is still inside) a description; the caller should skip it entirely.
fn strip_description<'a>(line: &'a str, in_description: &mut bool) -> Option<&'a str> {
    if *in_description {
        let idx = line.find("\"\"\"")?;
        *in_description = false;
        let rest = line[idx + 3..].trim();
        return (!rest.is_empty()).then_some(rest);
    }
    if let Some(rest) = line.strip_prefix("\"\"\"") {
        return match rest.find("\"\"\"") {
            None => {
                *in_description = true;
                None
            }
            Some(idx) => {
                let rest = rest[idx + 3..].trim();
                (!rest.is_empty()).then_some(rest)
            }
        };
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_yaml() {
        let spec = "\
openapi: 3.0.0
paths:
  /items:
    get:
      operationId: listItems
    post:
      operationId: createItem
  /items/{id}:
    get:
      operationId: getItem
    parameters: []
";
        let ops = parse(SpecKind::OpenApi, spec).unwrap();
        assert_eq!(ops.len(), 3);
        assert!(
            ops.iter()
                .any(|o| o.operation == "getItem" && o.method == "GET" && o.path == "/items/{id}")
        );
    }

    #[test]
    fn graphql_sdl() {
        let sdl = "\
type Item { id: ID! }

type Query {
  item(id: ID!): Item
  items: [Item!]!
}

type Mutation {
  createItem(name: String!): Item
}
";
        let ops = parse(SpecKind::GraphQl, sdl).unwrap();
        assert_eq!(ops.len(), 3);
        assert!(
            ops.iter()
                .any(|o| o.operation == "item" && o.method == "QUERY")
        );
        assert!(
            ops.iter()
                .any(|o| o.operation == "createItem" && o.method == "MUTATION")
        );
    }

    #[test]
    fn graphql_sdl_with_custom_root_type_names() {
        // async-graphql's merged-object pattern (and other generators)
        // commonly name root types after the Rust struct, not the GraphQL
        // canonical `Query`/`Subscription` — an explicit `schema { }` block
        // is how the SDL says which types those actually are.
        let sdl = "\
type QueryRoot {
  item(id: ID!): Item
}

type SubscriptionRoot {
  itemAdded: Item
}

schema {
  query: QueryRoot
  subscription: SubscriptionRoot
}
";
        let ops = parse(SpecKind::GraphQl, sdl).unwrap();
        assert_eq!(ops.len(), 2);
        assert!(
            ops.iter()
                .any(|o| o.operation == "item" && o.method == "QUERY")
        );
        assert!(
            ops.iter()
                .any(|o| o.operation == "itemAdded" && o.method == "SUBSCRIPTION")
        );
    }

    #[test]
    fn graphql_sdl_ignores_multiline_arg_lists_and_doc_comments() {
        // A field's own multi-line argument list (`items(\n  first: Int,\n
        // after: String\n): ...`) sits at the same brace depth as the field
        // itself — `first`/`after` must not be misread as sibling fields.
        // `"""..."""` descriptions (field- or arg-level) can contain
        // arbitrary prose with colons/parens that must not be read as SDL
        // syntax either.
        let sdl = "\
type Query {
  \"\"\"
  Batch items: paginated, cursor-based.
  \"\"\"
  items(
    \"\"\"
    Max items to return (e.g. 100); omitted = every match.
    \"\"\"
    first: Int,
    after: String
  ): ItemConnection!
  \"\"\" single-line description \"\"\"
  ping: Boolean!
}
";
        let ops = parse(SpecKind::GraphQl, sdl).unwrap();
        let names: Vec<&str> = ops.iter().map(|o| o.operation.as_str()).collect();
        assert_eq!(names, vec!["items", "ping"], "got {names:?}");
    }

    #[test]
    fn graphql_sdl_with_compact_single_line_schema_block() {
        // A whole-line `split_once(':')` would grab "schema { query" as the
        // key on this line and never see `mutation`, and the schema block's
        // braces net to zero on this one line too — both must not cause the
        // scan to stop before every root type is found.
        let sdl = "\
type QueryRoot {
  item: Item
}
type MutationRoot {
  createItem: Item
}
schema { query: QueryRoot, mutation: MutationRoot }
";
        let ops = parse(SpecKind::GraphQl, sdl).unwrap();
        assert!(
            ops.iter()
                .any(|o| o.operation == "item" && o.method == "QUERY")
        );
        assert!(
            ops.iter()
                .any(|o| o.operation == "createItem" && o.method == "MUTATION")
        );
    }

    #[test]
    fn graphql_sdl_with_directive_on_schema_block() {
        // Apollo Federation subgraphs commonly decorate the schema block
        // with a directive before its opening brace.
        let sdl = "\
type QueryRoot {
  item: Item
}
schema @link(url: \"https://specs.apollo.dev/link/v1.0\") {
  query: QueryRoot
}
";
        let ops = parse(SpecKind::GraphQl, sdl).unwrap();
        assert!(
            ops.iter()
                .any(|o| o.operation == "item" && o.method == "QUERY")
        );
    }

    #[test]
    fn graphql_sdl_ignores_schema_keyword_inside_a_doc_comment() {
        // Prose that happens to start a line with the word "schema" inside
        // a description must not be mistaken for a real schema block.
        let sdl = "\
\"\"\"
schema
query: NotARealType
\"\"\"
type Query {
  item: Item
}
";
        let ops = parse(SpecKind::GraphQl, sdl).unwrap();
        assert!(
            ops.iter()
                .any(|o| o.operation == "item" && o.method == "QUERY")
        );
    }

    #[test]
    fn graphql_sdl_keeps_content_after_a_closing_description_marker() {
        // A closing `"""` sharing a line with the next field is valid
        // formatter output and must not swallow that field.
        let sdl = "\
type Query {
  \"\"\"
  A field.
  \"\"\" ping: Boolean!
}
";
        let ops = parse(SpecKind::GraphQl, sdl).unwrap();
        let names: Vec<&str> = ops.iter().map(|o| o.operation.as_str()).collect();
        assert_eq!(names, vec!["ping"], "got {names:?}");
    }

    #[test]
    fn mcp_tools_json() {
        let spec = r#"{ "tools": [ { "name": "get_item", "inputSchema": {} }, { "name": "list_items" } ] }"#;
        let ops = parse(SpecKind::McpTools, spec).unwrap();
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].method, "TOOL");
    }

    #[test]
    fn asyncapi_channels() {
        let spec = "\
asyncapi: 2.6.0
channels:
  prices:
    subscribe:
      operationId: streamPrices
";
        let ops = parse(SpecKind::AsyncApi, spec).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].operation, "streamPrices");
        assert_eq!(ops[0].method, "SUBSCRIBE");
        assert_eq!(ops[0].path, "prices");
    }
}
