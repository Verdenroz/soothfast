//! Sidebar navigation: explicit groups from `[[site.nav]]`, or
//! auto-discovery from the docs tree when the config is silent. Nav is
//! plain data handed to the theme — how it looks is entirely the
//! `partials/nav.html` template's business.

use serde_json::{Value, json};

use crate::config::SiteConfig;

/// Minimal facts about a discovered page, gathered before rendering.
#[derive(Debug, Clone)]
pub struct PageMeta {
    /// Path relative to the docs dir ("perf/summary.md").
    pub src: String,
    /// Site route ("perf/summary/"; "" for index.md).
    pub route: String,
    /// First `#` heading, or the file stem.
    pub title: String,
}

/// Route for a docs-relative markdown path: pretty directory URLs.
pub fn route_for(src: &str) -> String {
    let stem = src.trim_end_matches(".md");
    if stem == "index" {
        return String::new();
    }
    if let Some(dir) = stem.strip_suffix("/index") {
        return format!("{dir}/");
    }
    format!("{stem}/")
}

/// What to do about a nav entry naming a page the build did not find.
///
/// The right answer depends on what the build is for. Publishing a sidebar
/// that links nowhere is drift and should fail. Writing docs is a different
/// activity: the page you are about to add does not exist yet, and stopping
/// the dev server over it helps nobody.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// Fail the build — what `docs build` does.
    Fail,
    /// Drop the entry and report it — what `docs serve` does.
    Warn,
}

/// Resolve nav groups to template data, plus any warnings raised.
///
/// Explicit nav wins over the automatic layout. A page it names that the
/// build did not find is handled per [`Missing`].
pub fn build(
    cfg: &SiteConfig,
    pages: &[PageMeta],
    missing: Missing,
) -> Result<(Value, Vec<String>), String> {
    if cfg.nav.is_empty() {
        return Ok((auto(pages), Vec::new()));
    }
    let mut warnings = Vec::new();
    let mut groups = Vec::new();
    for group in &cfg.nav {
        let mut items = Vec::new();
        for entry in &group.pages {
            // mkdocs-style explicit title: `"Home: index.md"` (paths never
            // contain `: `, so the split is unambiguous).
            let (title, src) = match entry.split_once(": ") {
                Some((t, p)) => (Some(t.trim()), p.trim()),
                None => (None, entry.as_str()),
            };
            let note = || format!("nav group {:?} lists missing page {src:?}", group.title);
            let Some(page) = pages.iter().find(|p| p.src == src) else {
                match missing {
                    Missing::Fail => return Err(note()),
                    Missing::Warn => {
                        warnings.push(note());
                        continue;
                    }
                }
            };
            let title = title.unwrap_or(&page.title);
            items.push(json!({ "title": title, "route": page.route, "src": page.src }));
        }
        groups.push(json!({ "title": group.title, "pages": items }));
    }
    Ok((Value::Array(groups), warnings))
}

/// Auto nav: root pages in one group (index first), then a group per
/// top-level subdirectory, everything alphabetical.
fn auto(pages: &[PageMeta]) -> Value {
    let mut root: Vec<&PageMeta> = pages.iter().filter(|p| !p.src.contains('/')).collect();
    root.sort_by_key(|p| (p.src != "index.md", p.src.clone()));
    let mut groups = Vec::new();
    if !root.is_empty() {
        groups.push(json!({
            "title": "Documentation",
            "pages": root.iter()
                .map(|p| json!({ "title": p.title, "route": p.route, "src": p.src }))
                .collect::<Vec<_>>(),
        }));
    }
    let mut dirs: Vec<String> = pages
        .iter()
        .filter_map(|p| p.src.split_once('/').map(|(d, _)| d.to_string()))
        .collect();
    dirs.sort();
    dirs.dedup();
    for dir in dirs {
        let mut in_dir: Vec<&PageMeta> = pages
            .iter()
            .filter(|p| p.src.starts_with(&format!("{dir}/")))
            .collect();
        in_dir.sort_by_key(|p| p.src.clone());
        groups.push(json!({
            "title": dir,
            "pages": in_dir.iter()
                .map(|p| json!({ "title": p.title, "route": p.route, "src": p.src }))
                .collect::<Vec<_>>(),
        }));
    }
    Value::Array(groups)
}

/// Copy of the nav with `current: true` stamped on the active page and a
/// ready-to-use `href` per page (relative to the page being rendered), so
/// the template highlights and links without any logic of its own.
pub fn with_current(nav: &Value, route: &str, base: &str) -> Value {
    let mut nav = nav.clone();
    if let Value::Array(groups) = &mut nav {
        for group in groups {
            if let Value::Array(pages) = &mut group["pages"] {
                for page in pages {
                    if page["route"] == route {
                        page["current"] = Value::Bool(true);
                    }
                    let target = page["route"].as_str().unwrap_or("");
                    page["href"] = Value::String(href(base, target));
                }
            }
        }
    }
    nav
}

/// `base` + `route`, with the empty result mapped to `./` (a valid link to
/// the current directory; an empty href reloads the page instead).
pub fn href(base: &str, route: &str) -> String {
    let joined = format!("{base}{route}");
    if joined.is_empty() {
        "./".into()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(src: &str, title: &str) -> PageMeta {
        PageMeta {
            src: src.into(),
            route: route_for(src),
            title: title.into(),
        }
    }

    #[test]
    fn routes_are_pretty_urls() {
        assert_eq!(route_for("index.md"), "");
        assert_eq!(route_for("measuring.md"), "measuring/");
        assert_eq!(route_for("perf/summary.md"), "perf/summary/");
        assert_eq!(route_for("perf/index.md"), "perf/");
    }

    #[test]
    fn explicit_nav_resolves_and_rejects_missing() {
        let pages = vec![meta("index.md", "Home"), meta("measuring.md", "Measuring")];
        let mut cfg = SiteConfig {
            nav: vec![crate::config::NavGroup {
                title: "Guide".into(),
                pages: vec!["measuring.md".into()],
            }],
            ..SiteConfig::default()
        };
        let (nav, _) = build(&cfg, &pages, Missing::Fail).unwrap();
        assert_eq!(nav[0]["pages"][0]["title"], "Measuring");
        // mkdocs-style explicit titles override the page's own h1.
        cfg.nav[0].pages.push("Home: index.md".into());
        let (nav, _) = build(&cfg, &pages, Missing::Fail).unwrap();
        assert_eq!(nav[0]["pages"][1]["title"], "Home");
        assert_eq!(nav[0]["pages"][1]["src"], "index.md");
        cfg.nav[0].pages.push("ghost.md".into());
        assert!(build(&cfg, &pages, Missing::Fail).is_err());
    }

    /// Writing docs and publishing them want opposite answers here: the
    /// page you are about to add does not exist yet.
    #[test]
    fn a_missing_page_fails_a_publish_but_only_warns_a_dev_build() {
        let pages = vec![meta("index.md", "Home")];
        let cfg = SiteConfig {
            nav: vec![crate::config::NavGroup {
                title: "Guide".into(),
                pages: vec!["index.md".into(), "not-written-yet.md".into()],
            }],
            ..SiteConfig::default()
        };
        assert!(build(&cfg, &pages, Missing::Fail).is_err());

        let (nav, warnings) = build(&cfg, &pages, Missing::Warn).expect("serve carries on");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("not-written-yet.md"), "{warnings:?}");
        // The absent entry is dropped, the rest of the group survives.
        assert_eq!(nav[0]["pages"].as_array().map(Vec::len), Some(1));
        assert_eq!(nav[0]["pages"][0]["src"], "index.md");
    }

    #[test]
    fn auto_nav_groups_root_then_dirs() {
        let pages = vec![
            meta("zeta.md", "Z"),
            meta("index.md", "Home"),
            meta("perf/summary.md", "Summary"),
        ];
        let (nav, _) = build(&SiteConfig::default(), &pages, Missing::Fail).unwrap();
        assert_eq!(nav[0]["title"], "Documentation");
        assert_eq!(nav[0]["pages"][0]["title"], "Home"); // index first
        assert_eq!(nav[1]["title"], "perf");
    }

    #[test]
    fn current_flag_and_hrefs_are_stamped() {
        let pages = vec![meta("index.md", "Home"), meta("a.md", "A")];
        let (nav, _) = build(&SiteConfig::default(), &pages, Missing::Fail).unwrap();
        let nav = with_current(&nav, "a/", "../");
        assert_eq!(nav[0]["pages"][1]["current"], true);
        assert!(nav[0]["pages"][0]["current"].is_null());
        assert_eq!(nav[0]["pages"][0]["href"], "../");
        assert_eq!(nav[0]["pages"][1]["href"], "../a/");
        assert_eq!(href("", ""), "./");
    }
}
