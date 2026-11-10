//! The public surface as the report engines consume it.

/// One public item as fed by the CLI (from the docs-engine surface).
pub struct SurfaceEntry {
    pub path: String,
    pub kind: String,
    pub signature: String,
    /// Name of the crate this item was extracted from.
    pub pkg: String,
    /// First line of the doc comment — compact, for search hits and lists.
    pub summary: String,
    /// Full doc comment (all paragraphs, code fences intact).
    pub docs: String,
}
