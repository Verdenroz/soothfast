//! soothfast-site — the native docs-site engine: markdown in, a themed,
//! evidence-carrying static site out. Replaces mkdocs in the soothfast
//! toolchain with zero new dependencies.
//!
//! Extension seams, from cheapest to deepest:
//! - `[site]` config in `soothfast.toml` (name, nav, extra_css/extra_js);
//! - `theme_dir`: overlay any theme file by relative path — one partial,
//!   one CSS token file, or the whole template set (`theme` module);
//! - `SitePlugin`: pipeline events for markdown/HTML rewriting, template
//!   context, and end-of-build outputs (`plugin` module). The built-in
//!   evidence chips and search index are themselves plugins.

pub mod build;
pub mod color;
pub mod config;
pub mod evidence;
pub mod highlight;
pub mod md;
pub mod nav;
pub mod plugin;
pub mod search;
pub mod serve;
pub mod template;
pub mod theme;
pub mod toml;

pub use build::{BuildInput, BuildReport, build};
pub use config::SiteConfig;
pub use plugin::SitePlugin;
