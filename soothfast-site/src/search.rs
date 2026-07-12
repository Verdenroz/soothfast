//! Built-in plugin: client-side search. Emits `search_index.json` — one
//! record per page section, format documented here and kept stable so a
//! custom theme (or an external indexer like pagefind) can replace the
//! bundled JS without touching the build.
//!
//! Record shape: `{ "route": "measuring/", "page": "Measuring",
//! "section": "Annotate", "anchor": "annotate", "text": "…" }`.

use std::path::Path;

use serde_json::{Value, json};

use crate::plugin::{PageRecord, SitePlugin};

/// Search index builder.
pub struct Search;

impl SitePlugin for Search {
    fn name(&self) -> &'static str {
        "search"
    }

    fn on_site_end(&self, pages: &[PageRecord], out_dir: &Path) -> Result<(), String> {
        let mut records = Vec::new();
        for page in pages {
            for section in split_sections(&page.html) {
                let text = strip_tags(&section.body);
                let text: String = text.chars().take(1200).collect();
                if text.trim().is_empty() && section.title.is_empty() {
                    continue;
                }
                records.push(json!({
                    "route": page.route,
                    "page": page.title,
                    "section": section.title,
                    "anchor": section.anchor,
                    "text": text.trim(),
                }));
            }
        }
        let path = out_dir.join("search_index.json");
        std::fs::write(&path, Value::Array(records).to_string())
            .map_err(|e| format!("cannot write {}: {e}", path.display()))
    }
}

struct Section {
    title: String,
    anchor: String,
    body: String,
}

/// Split a body fragment at `<h2 id="…">` boundaries; the preamble before
/// the first h2 is its own section with an empty title.
fn split_sections(html: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut rest = html;
    let mut current = Section {
        title: String::new(),
        anchor: String::new(),
        body: String::new(),
    };
    while let Some(at) = rest.find("<h2 id=\"") {
        current.body.push_str(&rest[..at]);
        sections.push(current);
        let after = &rest[at + "<h2 id=\"".len()..];
        let anchor = after.split('"').next().unwrap_or("").to_string();
        let title_end = after
            .find("<a class=\"anchor\"")
            .or_else(|| after.find("</h2>"));
        let title_start = after.find('>').map(|i| i + 1).unwrap_or(0);
        let title = match title_end {
            Some(end) if end > title_start => strip_tags(&after[title_start..end]),
            _ => String::new(),
        };
        let body_start = after.find("</h2>").map(|i| i + 5).unwrap_or(after.len());
        current = Section {
            title,
            anchor,
            body: String::new(),
        };
        rest = &after[body_start..];
    }
    current.body.push_str(rest);
    sections.push(current);
    sections
}

/// Plain text from an HTML fragment (tags dropped, entities left encoded
/// except the common few).
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#10;", " ")
        .replace("&amp;", "&");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_has_one_record_per_section() {
        let pages = vec![PageRecord {
            src: "a.md".into(),
            route: "a/".into(),
            title: "Alpha".into(),
            html: "<p>intro</p><h2 id=\"one\">One<a class=\"anchor\" href=\"#one\">#</a></h2>\
                   <p>first</p><h2 id=\"two\">Two</h2><p>second</p>"
                .into(),
        }];
        let dir = std::env::temp_dir().join(format!("soothfast-search-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        Search.on_site_end(&pages, &dir).unwrap();
        let raw = std::fs::read_to_string(dir.join("search_index.json")).unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        let recs = v.as_array().unwrap();
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[0]["section"], "");
        assert_eq!(recs[1]["section"], "One");
        assert_eq!(recs[1]["anchor"], "one");
        assert_eq!(recs[2]["text"], "second");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_tags_flattens_and_decodes() {
        assert_eq!(strip_tags("<p>a &lt;b&gt; <code>c</code></p>"), "a <b> c");
    }
}
