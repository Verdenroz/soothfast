//! What every generated dialect has in common, and how one is chosen.
//!
//! Generation is the same pipeline four times over — route shapes in, a
//! document out, a compatibility diff against the merge-base — so the CLI
//! holds a [`SpecKind`] and never a `match` over dialects. Adding a fifth
//! means writing an emitter and a diff, not touching the command.

use serde_json::Value;

use crate::SpecKind;
use crate::compat::Change;
use crate::schema::route_sig::RouteShape;
use crate::serialize;
use crate::{asyncapi, graphql, mcp, openapi};

/// Document metadata that no amount of code reading can derive.
#[derive(Debug, Clone, Default)]
pub struct Info {
    pub title: String,
    pub version: String,
    pub description: Option<String>,
    /// Server URLs. AsyncAPI splits these into host + protocol itself.
    pub servers: Vec<String>,
}

/// One operation to emit, pairing spec identity with the inferred shape.
#[derive(Debug, Clone)]
pub struct Operation {
    pub operation_id: String,
    /// HTTP verb, or QUERY/MUTATION/SUBSCRIPTION/SEND/RECEIVE/TOOL —
    /// whichever vocabulary the target dialect speaks.
    pub method: String,
    /// Route path, channel address, or field name.
    pub path: String,
    /// Usually the handler's doc comment.
    pub summary: Option<String>,
    pub shape: RouteShape,
}

/// A rendered document plus anything that had to be reconciled to build it.
#[derive(Debug, Clone, Default)]
pub struct Document {
    pub value: Value,
    /// Identity collisions. Never silently resolved: the last writer would
    /// otherwise win and quietly change the contract.
    pub conflicts: Vec<String>,
    /// Shapes the dialect cannot express as precisely as the code states
    /// them. Reported like extraction gaps, and like them never fatal.
    pub notes: Vec<String>,
}

impl SpecKind {
    /// Every dialect that can be generated, for help text and diagnostics.
    pub const ALL: &'static [SpecKind] = &[
        SpecKind::OpenApi,
        SpecKind::AsyncApi,
        SpecKind::GraphQl,
        SpecKind::McpTools,
    ];

    /// The name used in `soothfast.toml` and in messages.
    pub fn name(self) -> &'static str {
        match self {
            SpecKind::OpenApi => "openapi",
            SpecKind::AsyncApi => "asyncapi",
            SpecKind::GraphQl => "graphql",
            SpecKind::McpTools => "mcp",
        }
    }

    /// Parse a `dialect = "..."` value.
    pub fn parse_name(s: &str) -> Option<SpecKind> {
        SpecKind::ALL.iter().copied().find(|k| {
            k.name() == s
                // Spellings a reader would expect to work.
                || matches!(
                    (k, s),
                    (SpecKind::GraphQl, "graphql-sdl" | "sdl")
                        | (SpecKind::McpTools, "mcp-tools" | "tools")
                        | (SpecKind::OpenApi, "openapi-3.1")
                        | (SpecKind::AsyncApi, "asyncapi-3.0")
                )
        })
    }

    /// Guess the dialect of a spec file from its path.
    ///
    /// Generation needs a dialect before the file exists, so content sniffing
    /// (which [`crate::sniff_kind`] does on the reconcile path) is not
    /// available here. The guess is conservative and always overridable with
    /// an explicit `dialect =` key: OpenAPI is the default because it is what
    /// an unqualified `spec.yaml` almost always means.
    pub fn for_path(path: &str) -> SpecKind {
        let lower = path.to_ascii_lowercase();
        let file = lower.rsplit('/').next().unwrap_or(&lower);
        if file.ends_with(".graphql") || file.ends_with(".graphqls") || file.ends_with(".sdl") {
            return SpecKind::GraphQl;
        }
        if file.contains("asyncapi") {
            return SpecKind::AsyncApi;
        }
        if file.contains("mcp") || file.contains("tools") {
            return SpecKind::McpTools;
        }
        SpecKind::OpenApi
    }

    /// Assemble a document from inferred route shapes.
    pub fn document(self, info: &Info, ops: &[Operation]) -> Document {
        match self {
            SpecKind::OpenApi => openapi::document(info, ops),
            SpecKind::AsyncApi => asyncapi::document(info, ops),
            SpecKind::GraphQl => graphql::document(info, ops),
            SpecKind::McpTools => mcp::document(info, ops),
        }
    }

    /// Serialize a document for writing to `path`.
    ///
    /// GraphQL ignores the extension because SDL is its only serialization;
    /// the rest honour `.json` and default to the dialect's usual form.
    pub fn render(self, path: &str, value: &Value) -> String {
        if self == SpecKind::GraphQl {
            return graphql::to_sdl(value);
        }
        let json = match path.rsplit('.').next() {
            Some(ext) if ext.eq_ignore_ascii_case("json") => true,
            Some(ext) if ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml") => {
                false
            }
            // MCP tool manifests are JSON everywhere they are consumed.
            _ => self == SpecKind::McpTools,
        };
        if json {
            serialize::to_json(value)
        } else {
            serialize::to_yaml(value)
        }
    }

    /// Compare two revisions of a document for consumer compatibility.
    pub fn diff(self, old: &Value, new: &Value) -> Vec<Change> {
        match self {
            SpecKind::OpenApi => openapi::diff::diff(old, new),
            SpecKind::AsyncApi => asyncapi::diff::diff(old, new),
            SpecKind::GraphQl => graphql::diff::diff(old, new),
            SpecKind::McpTools => mcp::diff(old, new),
        }
    }

    /// The document a spec file that does not yet exist compares as, so that
    /// everything in a newly added file reads as additive by construction.
    pub fn empty(self) -> Value {
        match self {
            SpecKind::OpenApi => serde_json::json!({ "openapi": "3.1.0", "paths": {} }),
            SpecKind::AsyncApi => {
                serde_json::json!({ "asyncapi": "3.0.0", "channels": {}, "operations": {} })
            }
            SpecKind::GraphQl => serde_json::json!({ "types": {}, "roots": {} }),
            SpecKind::McpTools => serde_json::json!({ "tools": [] }),
        }
    }

    /// Operation and named-schema counts, for the one-line generation summary.
    pub fn summarize(self, value: &Value) -> (usize, usize) {
        let count = |v: &Value| v.as_object().map(|o| o.len()).unwrap_or(0);
        match self {
            SpecKind::OpenApi => (
                value["paths"]
                    .as_object()
                    .map(|p| p.values().map(count).sum())
                    .unwrap_or(0),
                count(&value["components"]["schemas"]),
            ),
            SpecKind::AsyncApi => (
                count(&value["operations"]),
                count(&value["components"]["schemas"]),
            ),
            SpecKind::GraphQl => (
                value["roots"]
                    .as_object()
                    .map(|r| r.values().map(count).sum())
                    .unwrap_or(0),
                count(&value["types"]),
            ),
            SpecKind::McpTools => (
                value["tools"].as_array().map(|a| a.len()).unwrap_or(0),
                value["tools"]
                    .as_array()
                    .map(|tools| {
                        tools
                            .iter()
                            .flat_map(|t| [&t["inputSchema"]["$defs"], &t["outputSchema"]["$defs"]])
                            .filter_map(|d| d.as_object())
                            .flat_map(|d| d.keys().cloned())
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                    })
                    .unwrap_or(0),
            ),
        }
    }
}

/// Report a method a dialect has no vocabulary for, rather than guessing one.
pub(crate) fn unknown_method(op: &Operation, dialect: &str, expected: &str) -> String {
    format!(
        "operation `{}` declares method `{}`, which {dialect} has no meaning for \
         (expected one of {expected}) — fix the #[route] method or point this \
         route at a spec file of the matching dialect",
        op.operation_id, op.method
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_spec_file_is_openapi() {
        assert_eq!(SpecKind::for_path("specs/spec.yaml"), SpecKind::OpenApi);
        assert_eq!(SpecKind::for_path("openapi.json"), SpecKind::OpenApi);
    }

    #[test]
    fn extensions_and_filenames_name_the_other_dialects() {
        assert_eq!(SpecKind::for_path("schema.graphql"), SpecKind::GraphQl);
        assert_eq!(SpecKind::for_path("a/b/schema.sdl"), SpecKind::GraphQl);
        assert_eq!(
            SpecKind::for_path("specs/asyncapi.yaml"),
            SpecKind::AsyncApi
        );
        assert_eq!(
            SpecKind::for_path("specs/mcp-tools.json"),
            SpecKind::McpTools
        );
        assert_eq!(SpecKind::for_path("tools.json"), SpecKind::McpTools);
    }

    #[test]
    fn a_directory_name_does_not_decide_the_dialect() {
        // `graphql/` as a directory is common; the file is what counts.
        assert_eq!(SpecKind::for_path("graphql/spec.yaml"), SpecKind::OpenApi);
    }

    #[test]
    fn dialect_names_round_trip() {
        for k in SpecKind::ALL {
            assert_eq!(SpecKind::parse_name(k.name()), Some(*k));
        }
        assert_eq!(SpecKind::parse_name("sdl"), Some(SpecKind::GraphQl));
        assert_eq!(SpecKind::parse_name("nonsense"), None);
    }

    #[test]
    fn the_extension_picks_the_serialization() {
        let v = serde_json::json!({ "openapi": "3.1.0" });
        assert!(SpecKind::OpenApi.render("a/spec.json", &v).starts_with('{'));
        assert!(
            SpecKind::OpenApi
                .render("a/spec.yaml", &v)
                .starts_with("openapi:")
        );
        // MCP defaults to JSON when the extension says nothing.
        let tools = serde_json::json!({ "tools": [] });
        assert!(SpecKind::McpTools.render("tools", &tools).starts_with('{'));
        assert!(
            SpecKind::McpTools
                .render("tools.yaml", &tools)
                .starts_with("tools:")
        );
    }

    #[test]
    fn an_empty_document_diffs_as_all_additive() {
        for kind in SpecKind::ALL {
            let empty = kind.empty();
            let changes = kind.diff(&empty, &empty);
            assert!(changes.is_empty(), "{kind:?} got {changes:?}");
        }
    }
}
