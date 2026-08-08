//! Soothfast SDK engine: native client emitters over the spec IR.
//!
//! Consumes the same pre-dialect operations (`dialect::Operation` +
//! `RouteShape`) the spec emitters render, and prints client source in
//! another language instead of a spec document. Emission is deterministic:
//! the same operations produce byte-identical output, so `sdk gen --check`
//! can gate staleness the way `spec gen --check` does.

pub mod envtemplate;
pub mod lower;
pub mod model;
mod naming;
mod python;
pub mod target;
mod typescript;

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
        let sdk = lower::lower(ops, opts, self)?;
        match self {
            SdkKind::Python => python::emit(info, &sdk, opts),
            SdkKind::TypeScript => typescript::emit(info, &sdk, opts),
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
    /// Default server; falls back to the first `Info` server. Optional
    /// when [`SdkOptions::embed`] supplies one at runtime instead.
    pub base_url: Option<String>,
    /// Server binary the package bundles and spawns, making the SDK
    /// self-contained: a client built with no base URL starts this and
    /// talks to it. `None` leaves the SDK a pure client for a hosted API.
    pub embed: Option<String>,
    /// Arguments passed to the embedded server on every launch.
    pub embed_args: Vec<String>,
    /// Environment applied to every launch, over the ambient one and under
    /// the caller's own. Config beats ambient deliberately: an inherited
    /// `PORT` from whatever runs the consumer's app must not decide where
    /// a bundled server binds.
    pub embed_env: BTreeMap<String, String>,
    /// The knobs the embedded server documents, parsed from its dotenv
    /// template. Becomes a typed `ServerEnv` on the client and a table in
    /// the README, so a client configures the same server the same way a
    /// deployment does.
    pub embed_env_vars: Vec<envtemplate::EnvVar>,
    /// Target triples the embedded server is built for. Empty means
    /// [`target::Target::DEFAULT`].
    pub targets: Vec<String>,
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
            embed: None,
            embed_args: Vec::new(),
            embed_env: BTreeMap::new(),
            embed_env_vars: Vec::new(),
            targets: Vec::new(),
            description: None,
            repository: None,
            paginated: Vec::new(),
            cursor_param: "cursor".into(),
            limit_param: "limit".into(),
        }
    }
}

/// The default base URL an emitter should bake in, if any.
///
/// An SDK that embeds its server does not need one — the launcher supplies
/// an address at runtime — so this is only fatal when neither is set.
fn base_url_for(info: &Info, opts: &SdkOptions) -> Result<Option<String>, String> {
    let base_url = opts
        .base_url
        .clone()
        .or_else(|| info.servers.first().cloned());
    if base_url.is_none() && opts.embed.is_none() {
        return Err(
            "no base URL: set `base_url` in the [[sdk]] entry, `servers` on the \
                    [[spec]] entry, or `embed` to bundle the server itself"
                .into(),
        );
    }
    Ok(base_url)
}

/// Rendered SDK files, keyed by path relative to the output directory.
#[derive(Debug, Clone, Default)]
pub struct SdkFileSet {
    pub files: BTreeMap<String, String>,
    /// Shapes the target language cannot express precisely.
    pub notes: Vec<String>,
}
