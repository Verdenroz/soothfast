//! The extension seam. Every feature that is not core rendering — evidence
//! chips, search, future user extensions — is a `SitePlugin` attached to
//! defined pipeline events, so the built-ins exercise the same API user
//! plugins will. Loading out-of-tree plugins (subprocess or wasm) can slot
//! in later by adapting them to this trait; the pipeline won't change.

use std::path::Path;

use serde_json::Value;

/// Identity of the page an event concerns.
pub struct PageRef<'a> {
    /// Source path relative to the docs dir ("measuring.md").
    pub src: &'a str,
    /// Site route ("measuring/"; "" for the front page).
    pub route: &'a str,
}

/// A fully rendered page, as seen by end-of-build events.
pub struct PageRecord {
    /// Source path relative to the docs dir.
    pub src: String,
    /// Site route ("" for the front page).
    pub route: String,
    /// Plain-text page title.
    pub title: String,
    /// Rendered body fragment (no chrome).
    pub html: String,
}

/// Pipeline events, in firing order. Default implementations are no-ops so
/// plugins implement only what they need.
pub trait SitePlugin {
    /// Stable name, for diagnostics and future config-based enable/disable.
    fn name(&self) -> &'static str;

    /// Rewrite raw markdown before the renderer sees it.
    fn on_page_markdown(&self, _page: &PageRef, md: String) -> Result<String, String> {
        Ok(md)
    }

    /// Rewrite the rendered HTML body fragment.
    fn on_page_html(&self, _page: &PageRef, html: String) -> Result<String, String> {
        Ok(html)
    }

    /// Add keys to the page's template context (e.g. `page.search_boost`).
    fn on_page_context(
        &self,
        _page: &PageRef,
        _ctx: &mut serde_json::Map<String, Value>,
    ) -> Result<(), String> {
        Ok(())
    }

    /// After every page is rendered: write extra outputs (indexes, feeds).
    fn on_site_end(&self, _pages: &[PageRecord], _out_dir: &Path) -> Result<(), String> {
        Ok(())
    }
}

/// Run one markdown-rewrite pass through all plugins, in order.
pub fn page_markdown(
    plugins: &[Box<dyn SitePlugin>],
    page: &PageRef,
    mut md: String,
) -> Result<String, String> {
    for p in plugins {
        md = p
            .on_page_markdown(page, md)
            .map_err(|e| format!("plugin {}: {e}", p.name()))?;
    }
    Ok(md)
}

/// Run one HTML-rewrite pass through all plugins, in order.
pub fn page_html(
    plugins: &[Box<dyn SitePlugin>],
    page: &PageRef,
    mut html: String,
) -> Result<String, String> {
    for p in plugins {
        html = p
            .on_page_html(page, html)
            .map_err(|e| format!("plugin {}: {e}", p.name()))?;
    }
    Ok(html)
}
