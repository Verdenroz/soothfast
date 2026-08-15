//! Soothfast spec engine, working in both directions.
//!
//! **Checking.** Anything that declares an external surface — an OpenAPI
//! spec, AsyncAPI channels, a GraphQL SDL schema, MCP tool schemas — parses
//! into a common [`DeclaredOp`] set ([`providers`]) and reconciles against
//! `#[route]` annotations with one mechanism ([`reconcile`]). The spec is
//! hand-authored and the code proves it, which is the only order that works
//! for a contract someone else serves.
//!
//! **Generating.** The same four dialects are also emitted *from* the code:
//! [`schema`] turns rustdoc JSON into JSON Schema, and one of [`openapi`],
//! [`asyncapi`], [`graphql`] or [`mcp`] renders a document from it. A
//! generated spec cannot disagree with the code, so what is left worth
//! gating is whether it broke a consumer — [`compat`], through each
//! dialect's own diff.
//!
//! [`SpecKind`] names the dialect in both directions, and [`dialect`] is
//! where the generation half dispatches on it.

pub mod asyncapi;
pub mod compat;
pub mod dialect;
pub mod graphql;
pub mod mcp;
pub mod openapi;
pub mod probe;
pub mod proto;
pub mod providers;
pub mod reconcile;
pub mod schema;
pub mod serialize;

/// One operation a spec declares.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredOp {
    /// Identity: operationId / GraphQL field / MCP tool / channel+verb.
    pub operation: String,
    /// HTTP verb, or QUERY/MUTATION/SUBSCRIPTION/PUBLISH/SUBSCRIBE/TOOL.
    pub method: String,
    /// Route path / field / tool / channel.
    pub path: String,
}

/// One `#[soothfast::route]` annotation from the code.
#[derive(Debug, Clone)]
pub struct RouteDecl {
    pub id: String,
    pub operation: String,
    pub method: String,
    pub path: String,
}

/// Spec dialects soothfast reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecKind {
    OpenApi,
    AsyncApi,
    GraphQl,
    McpTools,
}

/// Sniff the dialect from filename + content.
pub fn sniff_kind(filename: &str, text: &str) -> Option<SpecKind> {
    if filename.ends_with(".graphql") || filename.ends_with(".sdl") {
        return Some(SpecKind::GraphQl);
    }
    let head: String = text.chars().take(2000).collect();
    if head.contains("asyncapi") {
        Some(SpecKind::AsyncApi)
    } else if head.contains("openapi") || head.contains("swagger") {
        Some(SpecKind::OpenApi)
    } else if head.contains("\"tools\"") {
        Some(SpecKind::McpTools)
    } else if head.contains("type Query") || head.contains("type Mutation") {
        Some(SpecKind::GraphQl)
    } else {
        None
    }
}
