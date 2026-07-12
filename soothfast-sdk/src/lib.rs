//! Soothfast SDK engine: native client emitters over the spec IR.
//!
//! Consumes the same pre-dialect operations (`dialect::Operation` +
//! `RouteShape`) the spec emitters render, and prints client source in
//! another language instead of a spec document. Emission is deterministic:
//! the same operations produce byte-identical output, so `sdk gen --check`
//! can gate staleness the way `spec gen --check` does.

pub mod lower;
pub mod model;
mod python;

use std::collections::BTreeMap;

use soothfast_spec::dialect::{Info, Operation};

/// A language the SDK engine can emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdkKind {
    Python,
    TypeScript,
}

impl SdkKind {
    /// Every kind, for listings and error messages.
    pub const ALL: &'static [SdkKind] = &[SdkKind::Python, SdkKind::TypeScript];

    /// Canonical name, as `[[sdk]]` config spells it.
    pub fn name(self) -> &'static str {
        match self {
            SdkKind::Python => "python",
            SdkKind::TypeScript => "typescript",
        }
    }

    /// Parse a config name, accepting the common short forms.
    pub fn parse_name(s: &str) -> Option<SdkKind> {
        match s {
            "python" | "py" => Some(SdkKind::Python),
            "typescript" | "ts" => Some(SdkKind::TypeScript),
            _ => None,
        }
    }

    /// Emit a complete SDK package for these operations.
    pub fn emit(
        self,
        info: &Info,
        ops: &[Operation],
        opts: &SdkOptions,
    ) -> Result<SdkFileSet, String> {
        let sdk = lower::lower(ops, opts)?;
        match self {
            SdkKind::Python => python::emit(info, &sdk, opts),
            SdkKind::TypeScript => Err("the typescript emitter is not implemented yet".into()),
        }
    }
}

/// Everything an emitter needs that the operations don't carry.
#[derive(Debug, Clone)]
pub struct SdkOptions {
    /// Distribution name, e.g. `finance-query`.
    pub package: String,
    /// Import name, e.g. `finance_query`.
    pub module: String,
    pub version: String,
    /// Default server; falls back to the first `Info` server.
    pub base_url: Option<String>,
    pub description: Option<String>,
    pub repository: Option<String>,
    /// Operation ids whose response switches to a `{items, pageInfo}`
    /// envelope when `limit`/`cursor` are sent.
    pub paginated: Vec<String>,
    pub cursor_param: String,
    pub limit_param: String,
}

impl Default for SdkOptions {
    fn default() -> Self {
        SdkOptions {
            package: String::new(),
            module: String::new(),
            version: String::new(),
            base_url: None,
            description: None,
            repository: None,
            paginated: Vec::new(),
            cursor_param: "cursor".into(),
            limit_param: "limit".into(),
        }
    }
}

/// Rendered SDK files, keyed by path relative to the output directory.
#[derive(Debug, Clone, Default)]
pub struct SdkFileSet {
    pub files: BTreeMap<String, String>,
    /// Shapes the target language cannot express precisely.
    pub notes: Vec<String>,
}
