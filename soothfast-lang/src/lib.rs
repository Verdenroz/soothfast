//! Soothfast language engine.
//!
//! Finds declared symbols and their source spans in non-Rust sources with
//! tree-sitter, and fingerprints a symbol's span the way the docs engine
//! fingerprints rustdoc items, so prose can be bound to code in other
//! languages:
//! - [`symbols`] lists every declaration with its dotted container path;
//! - [`fingerprint`] hashes the normalized text of every span sharing a name.

mod classify;
mod fingerprint;
mod lang;
mod scan;

pub use fingerprint::fingerprint;
pub use lang::{Error, Lang};
pub use scan::{Symbol, SymbolKind, symbols};

#[cfg(test)]
mod tests;
