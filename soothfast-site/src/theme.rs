//! Theme = a named set of files (templates, partials, assets, icons). The
//! default "soothfast" theme is embedded in the binary; a config `theme_dir`
//! overlays files by relative path — override one partial or one CSS file
//! without forking the rest. This is the mkdocs `custom_dir` model.

use std::collections::BTreeMap;
use std::path::Path;

use crate::template::Includes;

/// Embedded default theme, keyed by theme-relative path.
const DEFAULT: &[(&str, &str)] = &[
    ("base.html", include_str!("../theme/base.html")),
    (
        "partials/header.html",
        include_str!("../theme/partials/header.html"),
    ),
    (
        "partials/nav.html",
        include_str!("../theme/partials/nav.html"),
    ),
    (
        "partials/toc.html",
        include_str!("../theme/partials/toc.html"),
    ),
    (
        "partials/footer.html",
        include_str!("../theme/partials/footer.html"),
    ),
    ("icons/logo.svg", include_str!("../theme/icons/logo.svg")),
    (
        "assets/tokens.css",
        include_str!("../theme/assets/tokens.css"),
    ),
    ("assets/site.css", include_str!("../theme/assets/site.css")),
    ("assets/site.js", include_str!("../theme/assets/site.js")),
    (
        "assets/favicon.svg",
        include_str!("../theme/assets/favicon.svg"),
    ),
];

/// A resolved theme: defaults plus user overrides.
pub struct Theme {
    overrides: BTreeMap<String, Vec<u8>>,
}

impl Theme {
    /// Load the default theme, overlaying every file under `override_dir`.
    pub fn load(override_dir: Option<&Path>) -> Result<Theme, String> {
        let mut overrides = BTreeMap::new();
        if let Some(dir) = override_dir {
            if !dir.is_dir() {
                return Err(format!("theme_dir {} is not a directory", dir.display()));
            }
            collect(dir, dir, &mut overrides)?;
        }
        Ok(Theme { overrides })
    }

    /// File bytes: override first, then embedded default.
    pub fn file(&self, name: &str) -> Option<Vec<u8>> {
        if let Some(bytes) = self.overrides.get(name) {
            return Some(bytes.clone());
        }
        DEFAULT
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, text)| text.as_bytes().to_vec())
    }

    /// File as UTF-8 text (templates); None if absent or not text.
    pub fn text(&self, name: &str) -> Option<String> {
        self.file(name)
            .and_then(|bytes| String::from_utf8(bytes).ok())
    }

    /// Every `assets/…` path the theme carries (defaults ∪ overrides),
    /// for copying into the built site.
    pub fn asset_paths(&self) -> Vec<String> {
        let mut names: Vec<String> = DEFAULT
            .iter()
            .map(|(n, _)| n.to_string())
            .filter(|n| n.starts_with("assets/"))
            .collect();
        for name in self.overrides.keys() {
            if name.starts_with("assets/") && !names.contains(name) {
                names.push(name.clone());
            }
        }
        names.sort();
        names
    }
}

impl Includes for Theme {
    fn template(&self, name: &str) -> Option<String> {
        self.text(name)
    }
}

fn collect(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("theme_dir {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(base, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(base)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            out.insert(rel, bytes);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_is_complete_and_templates_parse() {
        let theme = Theme::load(None).unwrap();
        for name in [
            "base.html",
            "partials/header.html",
            "partials/nav.html",
            "partials/toc.html",
            "partials/footer.html",
            "assets/tokens.css",
            "assets/site.css",
            "assets/site.js",
        ] {
            assert!(theme.text(name).is_some(), "missing theme file {name}");
        }
        // The base template must render against an empty-ish context.
        let ctx = serde_json::json!({
            "site": { "name": "t", "version": "0", "repo": "" },
            "page": { "title": "p", "route": "" },
            "base": "", "body": "<p>x</p>", "nav": [], "toc": [],
            "extra_css": [], "extra_js": [], "search": true,
        });
        let src = theme.text("base.html").unwrap();
        let html = crate::template::render(&src, &ctx, &theme).unwrap();
        assert!(html.contains("<p>x</p>"));
        assert!(html.starts_with("<!DOCTYPE html>"));
    }

    #[test]
    fn override_dir_shadows_defaults() {
        let dir = std::env::temp_dir().join(format!("soothfast-theme-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("partials"));
        std::fs::write(dir.join("partials/footer.html"), "MINE").unwrap();
        let theme = Theme::load(Some(&dir)).unwrap();
        assert_eq!(theme.text("partials/footer.html").unwrap(), "MINE");
        assert!(theme.text("base.html").is_some()); // default still present
        let _ = std::fs::remove_dir_all(&dir);
    }
}
