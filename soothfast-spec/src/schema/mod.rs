//! Rustdoc JSON → JSON Schema.
//!
//! Generation is rooted at `#[route]` handlers, and handler signatures are
//! monomorphic, so every type reachable from a route is concrete: generics
//! resolve by substitution, serde renames come through rustdoc's preserved
//! attributes, and foreign types resolve by canonical path against a table.
//!
//! Three things stay underivable, and all three are *detectable* — the
//! extractor never guesses a shape it cannot see:
//!
//! - **Erased returns** (`impl Trait`, `dyn Trait`) carry only a bound.
//! - **Custom serializers** (`with` / `serialize_with`) define the wire shape
//!   in code, so the field's Rust type stops predicting it.
//! - **Unmapped foreign types** are named exactly but cannot be walked.
//!
//! Each becomes a [`Gap`] and an open schema — imprecise, never wrong.

mod adt;
#[cfg(test)]
mod extract_tests;
pub mod foreign;
pub mod route_sig;
#[cfg(test)]
mod route_sig_tests;
pub mod serde_attrs;
pub mod types;

use std::collections::BTreeMap;

use serde_json::Value;

pub use foreign::TypeTable;
pub use route_sig::{Extractors, Overrides, RouteShape};
pub use types::Resolver;

/// A schema plus everything it referenced and everything it could not see.
#[derive(Debug, Clone)]
pub struct Extraction {
    /// The root schema, usually a `$ref` into `components`.
    pub schema: Value,
    /// Named schemas the root references, for `components/schemas`.
    pub components: BTreeMap<String, Value>,
    /// Places a shape could not be derived. Never empty *and* silent.
    pub gaps: Vec<Gap>,
}

/// Extract the schema for a named type from a rustdoc JSON document.
///
/// Used for the `#[route(response = "...")]` override; signature-driven
/// extraction goes through [`Resolver::resolve`] directly.
pub fn extract_named(doc: &Value, table: &TypeTable, name: &str) -> Result<Extraction, String> {
    let mut resolver = Resolver::new(doc, table)?;
    let id = resolver
        .find_type_id(name)
        .ok_or_else(|| format!("no struct or enum named `{name}` in this crate's public API"))?;
    let node = serde_json::json!({ "resolved_path": { "path": name, "id": id, "args": null } });
    let schema = resolver.resolve(&node, &types::Subst::new(), name);
    Ok(Extraction {
        schema,
        components: resolver.components,
        gaps: resolver.gaps,
    })
}

/// A place the extractor could not derive a shape, and why.
///
/// Every gap names the exact type or field so the report is actionable, and
/// carries an open schema rather than a guessed one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gap {
    /// `impl Trait` / `dyn Trait` in return position: only the bound survives.
    Erased {
        /// Where it appeared, e.g. `crate::routes::create_item`.
        at: String,
        /// The trait bound rustdoc did preserve.
        bound: String,
    },
    /// `#[serde(with = "...")]` — wire shape defined by code, not by type.
    CustomSerializer {
        at: String,
        /// The module or function named by the attribute.
        with: String,
    },
    /// A type outside this crate's rustdoc index with no table mapping.
    UnmappedForeign {
        at: String,
        /// Canonical path, e.g. `chrono::DateTime`.
        path: String,
    },
    /// A `rename_all` rule this extractor does not implement; emitting field
    /// names under an unknown rule would rename every one of them wrongly.
    UnknownRenameRule { at: String, rule: String },
    /// Rustdoc omits private fields, but serde still serializes them, so the
    /// field list is known to be incomplete.
    StrippedFields { at: String },
}

impl Gap {
    /// Where the gap was found, for grouping and reporting.
    pub fn at(&self) -> &str {
        match self {
            Self::Erased { at, .. }
            | Self::CustomSerializer { at, .. }
            | Self::UnmappedForeign { at, .. }
            | Self::UnknownRenameRule { at, .. }
            | Self::StrippedFields { at } => at,
        }
    }

    /// One-line explanation naming the cause and the remedy.
    pub fn explain(&self) -> String {
        match self {
            Self::Erased { at, bound } => format!(
                "{at}: returns an erased type (bound `{bound}`) — the concrete \
                 shape is not in rustdoc JSON; annotate `response = \"...\"` \
                 on #[route] or let runtime capture fill it"
            ),
            Self::CustomSerializer { at, with } => format!(
                "{at}: `#[serde(with = \"{with}\")]` defines the wire shape in \
                 code, so the field type does not predict it; emitted open"
            ),
            Self::UnmappedForeign { at, path } => format!(
                "{at}: foreign type `{path}` has no mapping; add one under \
                 [spec.types] in soothfast.toml"
            ),
            Self::UnknownRenameRule { at, rule } => format!(
                "{at}: unsupported `rename_all = \"{rule}\"`; field names left \
                 unrenamed rather than renamed wrongly"
            ),
            Self::StrippedFields { at } => format!(
                "{at}: has private fields that rustdoc omits but serde still \
                 serializes; emitted open (regenerate with private items \
                 documented for the full field set)"
            ),
        }
    }
}
