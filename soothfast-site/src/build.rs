//! The build pipeline: discover pages → plugin markdown pass → render →
//! plugin HTML pass → template → write, then assets and end-of-build
//! plugins. Deliberately boring glue; everything interesting lives behind
//! the seams (renderer hooks, theme files, plugins).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::config::SiteConfig;
use crate::nav::{self, PageMeta};
use crate::plugin::{PageRecord, PageRef, SitePlugin, page_html, page_markdown};
use crate::theme::Theme;
use crate::{md, template};

/// Everything a build needs; the CLI assembles this.
pub struct BuildInput {
    /// Directory `docs_dir`/`out_dir` are resolved against.
    pub root: PathBuf,
    /// The `[site]` config (defaults when `soothfast.toml` is absent).
    pub config: SiteConfig,
    /// Plugins, run in order at every pipeline event.
    pub plugins: Vec<Box<dyn SitePlugin>>,
}

/// What happened, for the CLI to report.
pub struct BuildReport {
    /// Pages rendered.
    pub pages: usize,
    /// Asset files written (theme + docs-dir + extras).
    pub assets: usize,
}

/// Build the whole site. Any page failing to render fails the build —
/// a half-broken published site is worse than a red command.
pub fn build(input: &BuildInput) -> Result<BuildReport, String> {
    let cfg = &input.config;
    let docs_dir = input.root.join(&cfg.docs_dir);
    let out_dir = input.root.join(&cfg.out_dir);
    prepare_out_dir(&out_dir, &input.root)?;
    let theme = Theme::load(
        cfg.theme_dir
            .as_ref()
            .map(|d| input.root.join(d))
            .as_deref(),
    )?;
    let theme_vars_css = if cfg.theme.is_default() {
        None
    } else {
        Some(
            crate::color::generate_theme_css(
                cfg.theme.primary.as_deref(),
                cfg.theme.secondary.as_deref(),
                cfg.theme.tertiary.as_deref(),
                cfg.theme.background.as_deref(),
            )
            .map_err(|e| format!("[site.theme]: {e}"))?,
        )
    };

    // 1. Render every page body first: titles and TOCs come from the real
    //    renderer (no parallel title scanner) and nav needs every title.
    let sources = find_md(&docs_dir, cfg, &input.root)?;
    let mut metas = Vec::new();
    let mut bodies = Vec::new(); // (body html, toc), parallel to metas
    for src in &sources {
        let text = read(&docs_dir.join(src))?;
        let route = nav::route_for(src);
        let page_ref = PageRef { src, route: &route };
        let rewritten = page_markdown(&input.plugins, &page_ref, text)?;

        let src_dir = parent(src);
        let depth = route.matches('/').count();
        let base = "../".repeat(depth);
        let link = |href: &str| rewrite_href(href, &src_dir, &base);
        let opts = md::Options {
            link: &link,
            code: &|_, _, _| None,
        };
        let rendered = md::render(&rewritten, &opts).map_err(|e| format!("{src}: {e}"))?;
        let body = page_html(&input.plugins, &page_ref, rendered.html)?;
        metas.push(PageMeta {
            src: src.clone(),
            route,
            title: rendered.title.unwrap_or_else(|| stem(src)),
        });
        bodies.push((body, rendered.toc));
    }
    let nav_data = nav::build(cfg, &metas)?;

    // 2. Wrap each body in the theme and write it.
    let mut records = Vec::new();
    let base_tpl = theme.text("base.html").ok_or("theme has no base.html")?;
    for (meta, (body, toc)) in metas.iter().zip(bodies) {
        let page_ref = PageRef {
            src: &meta.src,
            route: &meta.route,
        };
        let depth = meta.route.matches('/').count();
        let base = "../".repeat(depth);

        let mut ctx = serde_json::Map::new();
        ctx.insert(
            "site".into(),
            json!({
                "name": cfg.name,
                "version": cfg.version,
                "repo": cfg.repo,
                "logo": cfg.logo,
                "favicon": cfg.favicon,
            }),
        );
        ctx.insert(
            "page".into(),
            json!({ "title": meta.title, "route": meta.route, "src": meta.src }),
        );
        ctx.insert("base".into(), json!(base));
        ctx.insert("home".into(), json!(nav::href(&base, "")));
        ctx.insert("body".into(), json!(body));
        ctx.insert(
            "nav".into(),
            nav::with_current(&nav_data, &meta.route, &base),
        );
        ctx.insert(
            "toc".into(),
            Value::Array(
                toc.iter()
                    .map(|h| json!({ "level": h.level, "id": h.id, "text": h.text }))
                    .collect(),
            ),
        );
        ctx.insert("theme_vars".into(), json!(theme_vars_css.is_some()));
        ctx.insert("extra_css".into(), json!(cfg.extra_css));
        ctx.insert("extra_js".into(), json!(cfg.extra_js));
        ctx.insert("search".into(), json!(cfg.search));
        for plugin in &input.plugins {
            plugin
                .on_page_context(&page_ref, &mut ctx)
                .map_err(|e| format!("plugin {}: {e}", plugin.name()))?;
        }

        let html = template::render(&base_tpl, &Value::Object(ctx), &theme)
            .map_err(|e| format!("{}: base.html: {e}", meta.src))?;

        let out_path = out_dir.join(&meta.route).join("index.html");
        write(&out_path, html.as_bytes())?;
        records.push(PageRecord {
            src: meta.src.clone(),
            route: meta.route.clone(),
            title: meta.title.clone(),
            html: body,
        });
    }

    // 3. Assets: theme files, then docs-dir non-markdown files, then extras.
    let mut assets = 0;
    for name in theme.asset_paths() {
        let bytes = theme.file(&name).unwrap_or_default();
        let rel = name.trim_start_matches("assets/");
        write(&out_dir.join("_soothfast").join(rel), &bytes)?;
        assets += 1;
    }
    if let Some(css) = &theme_vars_css {
        write(&out_dir.join("_soothfast/theme-vars.css"), css.as_bytes())?;
        assets += 1;
    }
    assets += copy_docs_assets(&docs_dir, &out_dir, cfg, &input.root)?;
    for extra in cfg.extra_css.iter().chain(&cfg.extra_js) {
        let from = input.root.join(extra);
        let bytes =
            std::fs::read(&from).map_err(|e| format!("extra asset {}: {e}", from.display()))?;
        write(&out_dir.join(extra), &bytes)?;
        assets += 1;
    }

    // 4. End-of-build plugins (search index, future feeds/sitemaps).
    for plugin in &input.plugins {
        plugin
            .on_site_end(&records, &out_dir)
            .map_err(|e| format!("plugin {}: {e}", plugin.name()))?;
    }

    Ok(BuildReport {
        pages: records.len(),
        assets,
    })
}

/// Empty the output directory so removed pages don't linger as stale HTML.
/// Refuses to wipe anything that doesn't look like a built site — a typo'd
/// `out_dir` must not delete source trees.
fn prepare_out_dir(out_dir: &Path, root: &Path) -> Result<(), String> {
    if !out_dir.exists() {
        return Ok(());
    }
    if out_dir == root || out_dir.join("Cargo.toml").exists() {
        return Err(format!(
            "out_dir {} looks like a source directory, refusing to clean it",
            out_dir.display()
        ));
    }
    let empty = std::fs::read_dir(out_dir)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true);
    if empty {
        return Ok(());
    }
    // Any built-site fingerprint counts, including a partial mkdocs run.
    let looks_built = ["index.html", "404.html", "sitemap.xml", "_soothfast"]
        .iter()
        .any(|m| out_dir.join(m).exists());
    if !looks_built {
        return Err(format!(
            "out_dir {} is not empty and not a previous site build — remove it manually",
            out_dir.display()
        ));
    }
    std::fs::remove_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))
}

/// Markdown sources under the docs dir, relative, sorted; skips the theme
/// override dir and our own generated-fragment sniff line.
fn find_md(docs_dir: &Path, cfg: &SiteConfig, root: &Path) -> Result<Vec<String>, String> {
    if !docs_dir.is_dir() {
        return Err(format!("docs dir {} not found", docs_dir.display()));
    }
    let skip = cfg.theme_dir.as_ref().map(|d| root.join(d));
    let mut out = Vec::new();
    walk(docs_dir, docs_dir, skip.as_deref(), &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(base: &Path, dir: &Path, skip: Option<&Path>, out: &mut Vec<String>) -> Result<(), String> {
    if Some(dir) == skip {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(base, &path, skip, out)?;
        } else if path.extension().is_some_and(|e| e == "md") {
            let rel = path
                .strip_prefix(base)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
    Ok(())
}

fn copy_docs_assets(
    docs_dir: &Path,
    out_dir: &Path,
    cfg: &SiteConfig,
    root: &Path,
) -> Result<usize, String> {
    let skip = cfg.theme_dir.as_ref().map(|d| root.join(d));
    let mut copied = 0;
    let mut stack = vec![docs_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if Some(dir.as_path()) == skip.as_deref() {
            continue;
        }
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_none_or(|e| e != "md") {
                let rel = path.strip_prefix(docs_dir).map_err(|e| e.to_string())?;
                let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                write(&out_dir.join(rel), &bytes)?;
                copied += 1;
            }
        }
    }
    Ok(copied)
}

/// Rewrite a markdown href for the built site: `.md` links become routes,
/// other relative paths stay file paths; both get re-rooted via `base` so
/// pretty URLs (extra directory depth) can't break them.
fn rewrite_href(href: &str, src_dir: &str, base: &str) -> String {
    let external = href.starts_with("http://")
        || href.starts_with("https://")
        || href.starts_with("mailto:")
        || href.starts_with('/')
        || href.starts_with('#');
    if external {
        return href.to_string();
    }
    let (path, anchor) = match href.split_once('#') {
        Some((p, a)) => (p, format!("#{a}")),
        None => (href, String::new()),
    };
    let resolved = normalize(&format!("{src_dir}{path}"));
    if resolved.ends_with(".md") {
        return format!("{base}{}{anchor}", nav::route_for(&resolved));
    }
    format!("{base}{resolved}{anchor}")
}

/// Collapse `a/../b` and `./` segments in a relative path.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Directory prefix of a docs-relative path, with trailing slash ("" for root).
fn parent(src: &str) -> String {
    match src.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/"),
        None => String::new(),
    }
}

fn stem(src: &str) -> String {
    let name = src.rsplit('/').next().unwrap_or(src);
    name.trim_end_matches(".md").to_string()
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn href_rewriting_handles_md_assets_and_external() {
        // From docs/index.md (route "", depth 0).
        assert_eq!(rewrite_href("measuring.md", "", ""), "measuring/");
        assert_eq!(rewrite_href("perf/summary.md", "", ""), "perf/summary/");
        // From docs/perf/summary.md (route "perf/summary/", depth 2).
        let base = "../../";
        assert_eq!(
            rewrite_href("trend-allocs.svg", "perf/", base),
            "../../perf/trend-allocs.svg"
        );
        assert_eq!(rewrite_href("../index.md#run", "perf/", base), "../../#run");
        assert_eq!(
            rewrite_href("https://example.com", "perf/", base),
            "https://example.com"
        );
        assert_eq!(rewrite_href("#local", "perf/", base), "#local");
    }

    #[test]
    fn end_to_end_build_smoke() {
        let root = std::env::temp_dir().join(format!("soothfast-site-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("docs/sub")).unwrap();
        std::fs::write(
            root.join("docs/index.md"),
            "# Home\n\nSee [guide](sub/guide.md).\n\n```rust\nfn x() {}\n```\n",
        )
        .unwrap();
        std::fs::write(
            root.join("docs/sub/guide.md"),
            "# Guide\n\n## Part\n\nText.\n",
        )
        .unwrap();
        std::fs::write(root.join("docs/logo.svg"), "<svg></svg>").unwrap();

        let input = BuildInput {
            root: root.clone(),
            config: SiteConfig::default(),
            plugins: vec![Box::new(crate::search::Search)],
        };
        let report = build(&input).unwrap();
        assert_eq!(report.pages, 2);

        let home = std::fs::read_to_string(root.join("site/index.html")).unwrap();
        assert!(home.contains("<a href=\"sub/guide/\">guide</a>"));
        assert!(home.contains("tok-kw\">fn</span>"));
        assert!(home.contains("_soothfast/site.css"));
        let guide = std::fs::read_to_string(root.join("site/sub/guide/index.html")).unwrap();
        assert!(guide.contains("../../_soothfast/site.css")); // depth-aware base
        assert!(root.join("site/logo.svg").exists());
        assert!(root.join("site/search_index.json").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn site_theme_generates_and_links_theme_vars_css() {
        let root =
            std::env::temp_dir().join(format!("soothfast-site-theme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/index.md"), "# Home\n").unwrap();

        let mut cfg = SiteConfig::default();
        cfg.theme.primary = Some("#BF360C".into());
        let input = BuildInput {
            root: root.clone(),
            config: cfg,
            plugins: vec![Box::new(crate::search::Search)],
        };
        build(&input).unwrap();

        let home = std::fs::read_to_string(root.join("site/index.html")).unwrap();
        assert!(home.contains("_soothfast/theme-vars.css"));
        let css = std::fs::read_to_string(root.join("site/_soothfast/theme-vars.css")).unwrap();
        assert!(css.contains("--primary: #BF360C;"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn default_site_theme_does_not_link_theme_vars_css() {
        let root =
            std::env::temp_dir().join(format!("soothfast-site-notheme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/index.md"), "# Home\n").unwrap();

        let input = BuildInput {
            root: root.clone(),
            config: SiteConfig::default(),
            plugins: vec![Box::new(crate::search::Search)],
        };
        build(&input).unwrap();

        let home = std::fs::read_to_string(root.join("site/index.html")).unwrap();
        assert!(!home.contains("theme-vars.css"));
        assert!(!root.join("site/_soothfast/theme-vars.css").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
