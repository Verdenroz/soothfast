//! Strict markdown → HTML for the site: the subset soothfast docs actually
//! use (ATX headings, paragraphs, lists, tables, fences, blockquotes,
//! `!!!` admonitions, raw HTML passthrough). Constructs outside the subset
//! (reference links, setext headings) are build errors, not silent
//! mis-renders — the same stance the docs engine takes on drift.

use crate::highlight;
use crate::template::escape;

/// One collected heading, for the on-page table of contents.
#[derive(Debug, Clone)]
pub struct Heading {
    /// Heading depth (2 or 3; h1 is the page title, deeper levels skipped).
    pub level: u8,
    /// Anchor id, unique within the page.
    pub id: String,
    /// Plain-text heading (markup stripped).
    pub text: String,
}

/// A rendered page body.
#[derive(Debug)]
pub struct Rendered {
    /// Body fragment HTML (no page chrome).
    pub html: String,
    /// Plain text of the first `#` heading, if any.
    pub title: Option<String>,
    /// h2/h3 headings in document order.
    pub toc: Vec<Heading>,
}

/// Custom renderer for one fenced block: (lang, tags, code) → HTML, or
/// None to fall back to the default `<pre>` + highlight rendering.
pub type CodeHook<'a> = &'a dyn Fn(&str, &[String], &str) -> Option<String>;

/// Render hooks: link rewriting and custom code-block rendering, so the
/// build pipeline (and plugins) stay out of the parser.
pub struct Options<'a> {
    /// Rewrite an href (e.g. `measuring.md` → `../measuring/`).
    pub link: &'a dyn Fn(&str) -> String,
    /// Custom HTML for a fenced block; None falls back to `<pre>` + highlight.
    pub code: CodeHook<'a>,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options {
            link: &|href| href.to_string(),
            code: &|_, _, _| None,
        }
    }
}

/// Render one markdown document.
pub fn render(md: &str, opts: &Options) -> Result<Rendered, String> {
    let lines: Vec<&str> = md.lines().collect();
    let mut st = State {
        out: String::with_capacity(md.len() * 2),
        title: None,
        toc: Vec::new(),
        ids: std::collections::BTreeMap::new(),
        tab_groups: 0,
        tab_panel_depth: 0,
    };
    let mut i = 0;
    while i < lines.len() {
        i = block(&lines, i, &mut st, opts)?;
    }
    Ok(Rendered {
        html: st.out,
        title: st.title,
        toc: st.toc,
    })
}

struct State {
    out: String,
    title: Option<String>,
    toc: Vec<Heading>,
    /// slug → count, for unique heading ids.
    ids: std::collections::BTreeMap<String, usize>,
    /// Tab groups seen so far, for unique radio-input names.
    tab_groups: usize,
    /// Nesting depth inside `tabs()` panel bodies. Only one panel is visible
    /// at a time (CSS radio switching, no JS), so headings in here aren't
    /// independently reachable and must not appear in the page TOC — but
    /// they still get their own anchor `id` for direct linking.
    tab_panel_depth: usize,
}

/// Render the block starting at line `i`; returns the next unconsumed line.
fn block(lines: &[&str], i: usize, st: &mut State, opts: &Options) -> Result<usize, String> {
    let line = lines[i];
    let trimmed = line.trim_start();

    if trimmed.is_empty() {
        return Ok(i + 1);
    }

    // HTML comments (soothfast markers are rewritten before rendering; any
    // survivors — closing markers, author notes — are dropped).
    if trimmed.starts_with("<!--") {
        let mut j = i;
        while j < lines.len() && !lines[j].contains("-->") {
            j += 1;
        }
        return Ok(j + 1);
    }

    // Raw HTML block: pass through until a blank line — except
    // script/style/pre/textarea, which read until their closing tag.
    // Those routinely contain blank lines (e.g. CSS rule groups) that
    // must not truncate the block early.
    if let Some(rest) = trimmed.strip_prefix('<') {
        let tag: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        let mut j = i;
        if matches!(
            tag.to_ascii_lowercase().as_str(),
            "script" | "style" | "pre" | "textarea"
        ) {
            let close = format!("</{}", tag.to_ascii_lowercase());
            while j < lines.len() {
                let has_close = lines[j].to_ascii_lowercase().contains(&close);
                st.out.push_str(lines[j]);
                st.out.push('\n');
                j += 1;
                if has_close {
                    break;
                }
            }
        } else {
            while j < lines.len() && !lines[j].trim().is_empty() {
                st.out.push_str(lines[j]);
                st.out.push('\n');
                j += 1;
            }
        }
        return Ok(j);
    }

    // Fenced code.
    let ticks = trimmed.chars().take_while(|&c| c == '`').count();
    if ticks >= 3 {
        return fence(lines, i, ticks, st, opts);
    }

    // ATX heading.
    if let Some(rest) = heading_text(trimmed) {
        let (level, text) = rest;
        let id = st.unique_id(&slugify(&plain_text(&text)));
        let inline = inline(&text, opts)?;
        if level == 1 && st.title.is_none() {
            // Titles feed plain-text contexts (nav, <title>): strip markup.
            st.title = Some(plain_text(&text));
        }
        if (2..=3).contains(&level) && st.tab_panel_depth == 0 {
            st.toc.push(Heading {
                level,
                id: id.clone(),
                text: plain_text(&text),
            });
        }
        st.out.push_str(&format!(
            "<h{level} id=\"{id}\">{inline}<a class=\"anchor\" href=\"#{id}\" aria-label=\"Link to this section\">#</a></h{level}>\n"
        ));
        return Ok(i + 1);
    }

    // Setext underlines are easy to write accidentally and we don't support
    // them: refuse rather than silently render a stray paragraph.
    if i + 1 < lines.len() {
        let next = lines[i + 1].trim();
        if !next.is_empty()
            && next.len() >= 3
            && (next.chars().all(|c| c == '=') || next.chars().all(|c| c == '-'))
            && !line.trim().is_empty()
            && !is_list_item(trimmed)
        {
            return Err(format!(
                "line {}: setext heading underline — use `#` headings",
                i + 2
            ));
        }
    }

    // Reference-style link definition.
    if trimmed.starts_with('[') && trimmed.contains("]:") && !trimmed.contains("](") {
        return Err(format!(
            "line {}: reference-style links are unsupported — use inline [text](url)",
            i + 1
        ));
    }

    // Horizontal rule.
    if trimmed.len() >= 3
        && (trimmed.chars().all(|c| c == '-')
            || trimmed.chars().all(|c| c == '*')
            || trimmed.chars().all(|c| c == '_'))
    {
        st.out.push_str("<hr>\n");
        return Ok(i + 1);
    }

    // Admonition: `!!! note "Optional title"` with a 4-space-indented body.
    if let Some(rest) = trimmed.strip_prefix("!!! ") {
        return admonition(lines, i, rest, st, opts);
    }

    // Content tabs: `=== "Title"` with 4-space-indented bodies (the mkdocs
    // pymdownx.tabbed syntax), rendered as CSS-only radio tabs.
    if tab_title(trimmed).is_some() {
        return tabs(lines, i, st, opts);
    }

    // Blockquote.
    if trimmed.starts_with('>') {
        let mut inner = Vec::new();
        let mut j = i;
        while j < lines.len() {
            let t = lines[j].trim_start();
            let Some(rest) = t.strip_prefix('>') else {
                break;
            };
            inner.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            j += 1;
        }
        let inner_refs: Vec<&str> = inner.iter().map(String::as_str).collect();
        st.out.push_str("<blockquote>\n");
        let mut k = 0;
        while k < inner_refs.len() {
            k = block(&inner_refs, k, st, opts)?;
        }
        st.out.push_str("</blockquote>\n");
        return Ok(j);
    }

    // List.
    if is_list_item(trimmed) {
        return list(lines, i, st, opts);
    }

    // Table: a pipe row followed by a separator row.
    if trimmed.starts_with('|') && i + 1 < lines.len() && is_table_sep(lines[i + 1]) {
        return table(lines, i, st, opts);
    }

    // Paragraph: gather until blank line or a new block form.
    let mut text = String::new();
    let mut j = i;
    while j < lines.len() {
        let t = lines[j].trim_start();
        if t.is_empty()
            || t.starts_with("<!--")
            || t.starts_with('#') && heading_text(t).is_some()
            || t.chars().take_while(|&c| c == '`').count() >= 3
            || is_list_item(t)
            || t.starts_with('>')
            || t.starts_with("!!! ")
            || html_block_start(t)
        {
            break;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(lines[j].trim());
        j += 1;
    }
    st.out.push_str("<p>");
    st.out.push_str(&inline(&text, opts)?);
    st.out.push_str("</p>\n");
    Ok(j)
}

/// Block-level raw HTML interrupts a paragraph, so plugin-emitted chips and
/// panels directly after a prose line aren't absorbed and double-escaped.
fn html_block_start(t: &str) -> bool {
    let Some(rest) = t.strip_prefix('<') else {
        return false;
    };
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let tag: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    matches!(
        tag.to_ascii_lowercase().as_str(),
        "div"
            | "figure"
            | "table"
            | "pre"
            | "blockquote"
            | "ul"
            | "ol"
            | "p"
            | "aside"
            | "section"
            | "details"
            | "hr"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
    )
}

/// `## Heading` → (2, "Heading"); trailing `#`s trimmed per CommonMark.
fn heading_text(trimmed: &str) -> Option<(u8, String)> {
    let level = trimmed.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = trimmed[level..].strip_prefix(' ')?;
    Some((level as u8, rest.trim_end_matches(['#', ' ']).to_string()))
}

fn fence(
    lines: &[&str],
    i: usize,
    ticks: usize,
    st: &mut State,
    opts: &Options,
) -> Result<usize, String> {
    let trimmed = lines[i].trim_start();
    let info = trimmed[ticks..].trim();
    let mut tokens = info.split_whitespace().map(str::to_string);
    let lang = tokens.next().unwrap_or_default();
    let tags: Vec<String> = tokens.collect();

    let mut code = String::new();
    let mut j = i + 1;
    loop {
        if j >= lines.len() {
            return Err(format!("line {}: unclosed code fence", i + 1));
        }
        let t = lines[j].trim_start();
        if t.chars().take_while(|&c| c == '`').count() >= ticks
            && t.trim_end().chars().all(|c| c == '`')
        {
            break;
        }
        code.push_str(lines[j]);
        code.push('\n');
        j += 1;
    }

    if let Some(html) = (opts.code)(&lang, &tags, &code) {
        st.out.push_str(&html);
    } else {
        st.out.push_str(&code_html(&lang, &tags, &code));
    }
    st.out.push('\n');
    Ok(j + 1)
}

/// Default fenced-block rendering: instrument-panel `<figure>` with a
/// language label and highlighted body.
pub fn code_html(lang: &str, tags: &[String], code: &str) -> String {
    let label = if tags.is_empty() {
        lang.to_string()
    } else {
        format!("{lang} · {}", tags.join(" "))
    };
    let body = highlight::highlight(lang, code);
    format!(
        "<figure class=\"code\"><figcaption class=\"code-bar\"><span class=\"code-lang\">{}</span>\
<button class=\"code-copy\" type=\"button\" data-copy>Copy</button></figcaption>\
<pre><code>{}</code></pre></figure>",
        escape(&label),
        body
    )
}

fn admonition(
    lines: &[&str],
    i: usize,
    rest: &str,
    st: &mut State,
    opts: &Options,
) -> Result<usize, String> {
    let (kind, title) = match rest.split_once(' ') {
        Some((k, t)) => (k, t.trim().trim_matches('"').to_string()),
        None => (rest.trim(), String::new()),
    };
    // Every non-severity admonition kind (abstract/info/note/tip/…) shares
    // the same box color by design — color is reserved for evidence, not
    // decoration. Without the kind spelled out, a custom title reads as
    // just another identically-styled blue box; a reader can't tell a
    // low-stakes "see also" note from a "you must configure this" tip
    // without reading the prose. So: prefix a custom title with its kind
    // (reusing the `lang · tag` label grammar code blocks already use);
    // when there's no custom title, the kind alone is the whole label.
    let title_html = if title.is_empty() {
        escape(kind)
    } else {
        format!(
            "<span class=\"adm-kind\">{}</span> · {}",
            escape(kind),
            escape(&title)
        )
    };
    let mut body: Vec<String> = Vec::new();
    let mut j = i + 1;
    while j < lines.len() {
        let l = lines[j];
        if l.trim().is_empty() {
            // Blank line inside the body only if more indented content follows.
            if lines.get(j + 1).is_some_and(|n| n.starts_with("    ")) {
                body.push(String::new());
                j += 1;
                continue;
            }
            break;
        }
        let Some(stripped) = l.strip_prefix("    ") else {
            break;
        };
        body.push(stripped.to_string());
        j += 1;
    }
    st.out.push_str(&format!(
        "<div class=\"adm adm-{}\"><p class=\"adm-title\">{}</p>\n",
        escape(kind),
        title_html
    ));
    let body_refs: Vec<&str> = body.iter().map(String::as_str).collect();
    let mut k = 0;
    while k < body_refs.len() {
        k = block(&body_refs, k, st, opts)?;
    }
    st.out.push_str("</div>\n");
    Ok(j)
}

/// `=== "Title"` → Some("Title").
fn tab_title(trimmed: &str) -> Option<&str> {
    let rest = trimmed.strip_prefix("=== ")?.trim();
    rest.strip_prefix('"')?.strip_suffix('"')
}

/// One run of consecutive `=== "Title"` tabs with indented bodies. Emits a
/// radio-input tab group: all inputs+labels first (the tab bar), then one
/// `<section>` per body — pure CSS switching, no JS.
fn tabs(lines: &[&str], start: usize, st: &mut State, opts: &Options) -> Result<usize, String> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut j = start;
    while j < lines.len() {
        let t = lines[j].trim_start();
        if let Some(title) = tab_title(t) {
            groups.push((title.to_string(), Vec::new()));
            j += 1;
            continue;
        }
        if lines[j].trim().is_empty() {
            // A blank line stays in the body only if the group continues.
            let continues = lines
                .get(j + 1)
                .is_some_and(|n| n.starts_with("    ") || tab_title(n.trim_start()).is_some());
            if continues && !groups.is_empty() {
                if let Some(g) = groups.last_mut() {
                    g.1.push(String::new());
                }
                j += 1;
                continue;
            }
            break;
        }
        let Some(stripped) = lines[j].strip_prefix("    ") else {
            break;
        };
        let Some(g) = groups.last_mut() else { break };
        g.1.push(stripped.to_string());
        j += 1;
    }

    st.tab_groups += 1;
    let group = st.tab_groups;
    st.out.push_str("<div class=\"tabs\">\n");
    for (k, (title, _)) in groups.iter().enumerate() {
        let checked = if k == 0 { " checked" } else { "" };
        st.out.push_str(&format!(
            "<input type=\"radio\" name=\"tabs-{group}\" id=\"tab-{group}-{k}\"{checked}>\
<label for=\"tab-{group}-{k}\">{}</label>\n",
            escape(title)
        ));
    }
    for (_, body) in &groups {
        st.out.push_str("<section class=\"tab-panel\">\n");
        let body_refs: Vec<&str> = body.iter().map(String::as_str).collect();
        st.tab_panel_depth += 1;
        let mut k = 0;
        while k < body_refs.len() {
            k = block(&body_refs, k, st, opts)?;
        }
        st.tab_panel_depth -= 1;
        st.out.push_str("</section>\n");
    }
    st.out.push_str("</div>\n");
    Ok(j)
}

fn is_list_item(trimmed: &str) -> bool {
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        return !rest.is_empty();
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && trimmed[digits..].starts_with(". ")
}

/// Marker-stripped list item text and whether the list is ordered.
fn item_text(trimmed: &str) -> (bool, &str) {
    for p in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(p) {
            return (false, rest);
        }
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    (true, &trimmed[digits + 2..])
}

fn list(lines: &[&str], start: usize, st: &mut State, opts: &Options) -> Result<usize, String> {
    // (indent, ordered) stack of open lists.
    let mut stack: Vec<(usize, bool)> = Vec::new();
    let mut open_item = false;
    let mut j = start;
    while j < lines.len() {
        let line = lines[j];
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            // A blank line ends the list unless another item follows directly.
            if lines.get(j + 1).map(|l| is_list_item(l.trim_start())) == Some(true) {
                j += 1;
                continue;
            }
            break;
        }
        let indent = line.len() - trimmed.len();
        if !is_list_item(trimmed) {
            // Continuation line of the current item.
            if open_item && indent > stack.last().map_or(0, |s| s.0) {
                st.out.push(' ');
                st.out.push_str(&inline(trimmed, opts)?);
                j += 1;
                continue;
            }
            break;
        }
        let (ordered, text) = item_text(trimmed);
        while stack.last().is_some_and(|&(ind, _)| indent < ind) {
            close_list(&mut stack, &mut open_item, st);
        }
        match stack.last().copied() {
            Some((ind, _)) if indent > ind => {
                st.out.push_str(if ordered { "<ol>\n" } else { "<ul>\n" });
                stack.push((indent, ordered));
            }
            // Same level but the marker kind changed: a new list starts.
            Some((_, was_ordered)) if was_ordered != ordered => {
                close_list(&mut stack, &mut open_item, st);
                st.out.push_str(if ordered { "<ol>\n" } else { "<ul>\n" });
                stack.push((indent, ordered));
            }
            None => {
                st.out.push_str(if ordered { "<ol>\n" } else { "<ul>\n" });
                stack.push((indent, ordered));
            }
            Some(_) => {
                if open_item {
                    st.out.push_str("</li>\n");
                }
            }
        }
        st.out.push_str("<li>");
        st.out.push_str(&inline(text, opts)?);
        open_item = true;
        j += 1;
    }
    while !stack.is_empty() {
        close_list(&mut stack, &mut open_item, st);
    }
    Ok(j)
}

fn close_list(stack: &mut Vec<(usize, bool)>, open_item: &mut bool, st: &mut State) {
    if *open_item {
        st.out.push_str("</li>\n");
        *open_item = false;
    }
    if let Some((_, ordered)) = stack.pop() {
        st.out.push_str(if ordered { "</ol>\n" } else { "</ul>\n" });
    }
    // Closing a nested list still leaves the parent's item open.
    *open_item = !stack.is_empty();
}

fn is_table_sep(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn table(lines: &[&str], start: usize, st: &mut State, opts: &Options) -> Result<usize, String> {
    let cells = |line: &str| -> Vec<String> {
        line.trim()
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect()
    };
    let aligns: Vec<&str> = cells(lines[start + 1])
        .iter()
        .map(|c| {
            let l = c.starts_with(':');
            let r = c.ends_with(':');
            match (l, r) {
                (true, true) => " style=\"text-align:center\"",
                (false, true) => " style=\"text-align:right\"",
                _ => "",
            }
        })
        .collect();

    st.out
        .push_str("<div class=\"tablewrap\"><table>\n<thead><tr>");
    for (n, h) in cells(lines[start]).iter().enumerate() {
        let a = aligns.get(n).copied().unwrap_or("");
        st.out
            .push_str(&format!("<th{a}>{}</th>", inline(h, opts)?));
    }
    st.out.push_str("</tr></thead>\n<tbody>\n");
    let mut j = start + 2;
    while j < lines.len() && lines[j].trim_start().starts_with('|') {
        st.out.push_str("<tr>");
        for (n, c) in cells(lines[j]).iter().enumerate() {
            let a = aligns.get(n).copied().unwrap_or("");
            st.out
                .push_str(&format!("<td{a}>{}</td>", inline(c, opts)?));
        }
        st.out.push_str("</tr>\n");
        j += 1;
    }
    st.out.push_str("</tbody>\n</table></div>\n");
    Ok(j)
}

/// Inline rendering: code spans, strong/em, links, images, autolinks, raw
/// inline HTML passthrough. Docs are first-party content — inline HTML is
/// trusted, matching mkdocs' `md_in_html` behavior.
fn inline(text: &str, opts: &Options) -> Result<String, String> {
    let b: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            '\\' if i + 1 < b.len() => {
                out.push_str(&escape(&b[i + 1].to_string()));
                i += 2;
            }
            '`' => {
                let ticks = count(&b, i, '`');
                if let Some(end) = find_run(&b, i + ticks, '`', ticks) {
                    let code: String = b[i + ticks..end].iter().collect();
                    out.push_str("<code>");
                    out.push_str(&escape(code.trim_matches(' ')));
                    out.push_str("</code>");
                    i = end + ticks;
                } else {
                    out.push('`');
                    i += 1;
                }
            }
            '*' => {
                let stars = count(&b, i, '*').min(2);
                if let Some(end) = find_run(&b, i + stars, '*', stars) {
                    let innr: String = b[i + stars..end].iter().collect();
                    let tag = if stars == 2 { "strong" } else { "em" };
                    out.push_str(&format!("<{tag}>{}</{tag}>", inline(&innr, opts)?));
                    i = end + stars;
                } else {
                    out.push('*');
                    i += 1;
                }
            }
            '!' if b.get(i + 1) == Some(&'[') => match link_parts(&b, i + 1) {
                Some((alt, href, end)) => {
                    out.push_str(&format!(
                        "<img src=\"{}\" alt=\"{}\">",
                        escape(&(opts.link)(&href)),
                        escape(&alt)
                    ));
                    i = end;
                }
                None => {
                    out.push('!');
                    i += 1;
                }
            },
            '[' => match link_parts(&b, i) {
                Some((label, href, end)) => {
                    out.push_str(&format!(
                        "<a href=\"{}\">{}</a>",
                        escape(&(opts.link)(&href)),
                        inline(&label, opts)?
                    ));
                    i = end;
                }
                None => {
                    out.push('[');
                    i += 1;
                }
            },
            '<' => {
                // Autolink or raw inline HTML tag; bare `<` is escaped.
                // `find` returns byte offsets but `i` indexes chars: advance
                // by the consumed slice's char count, not by `gt`.
                let rest: String = b[i..].iter().collect();
                if rest.starts_with("<http")
                    && let Some(gt) = rest.find('>')
                {
                    let url = &rest[1..gt];
                    out.push_str(&format!("<a href=\"{0}\">{0}</a>", escape(url)));
                    i += rest[..=gt].chars().count();
                    continue;
                }
                let is_tag = b
                    .get(i + 1)
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == '/');
                if is_tag && let Some(gt) = rest.find('>') {
                    out.push_str(&rest[..=gt]);
                    i += rest[..=gt].chars().count();
                } else {
                    out.push_str("&lt;");
                    i += 1;
                }
            }
            '&' => {
                out.push_str("&amp;");
                i += 1;
            }
            '\n' => {
                out.push('\n');
                i += 1;
            }
            c => {
                if c == '>' {
                    out.push_str("&gt;");
                } else {
                    out.push(c);
                }
                i += 1;
            }
        }
    }
    Ok(out)
}

/// `[label](href)` or `[label](href "title")` starting at `[`; returns
/// (label, href, index past `)`).
fn link_parts(b: &[char], start: usize) -> Option<(String, String, usize)> {
    let mut depth = 0;
    let mut close = None;
    for (n, &c) in b.iter().enumerate().skip(start) {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(n);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    if b.get(close + 1) != Some(&'(') {
        return None;
    }
    // Count depth: a wiki-style `..._(complexity)` URL ends at its own `)`.
    let mut depth = 1;
    let mut end = None;
    for (n, &c) in b.iter().enumerate().skip(close + 2) {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(n);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let dest: String = b[close + 2..end].iter().collect();
    Some((
        b[start + 1..close].iter().collect(),
        strip_title(dest.trim()),
        end + 1,
    ))
}

/// A destination may carry a `"title"` the renderer has no slot for;
/// keeping it would land quoted prose inside the `href` attribute.
fn strip_title(dest: &str) -> String {
    for quote in ['"', '\''] {
        if let Some((href, title)) = dest.split_once(quote)
            && title.ends_with(quote)
        {
            return href.trim().to_string();
        }
    }
    dest.to_string()
}

fn count(b: &[char], i: usize, c: char) -> usize {
    b[i..].iter().take_while(|&&x| x == c).count()
}

/// Index of the next run of exactly-or-more `n` × `c`, if any.
fn find_run(b: &[char], from: usize, c: char, n: usize) -> Option<usize> {
    let mut i = from;
    while i < b.len() {
        if b[i] == c && count(b, i, c) >= n {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Heading text for plain-text contexts (TOC): code ticks and inline HTML
/// tags dropped.
fn plain_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            '`' => {}
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn slugify(text: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

impl State {
    fn unique_id(&mut self, slug: &str) -> String {
        let slug = if slug.is_empty() { "section" } else { slug };
        let n = self.ids.entry(slug.to_string()).or_insert(0);
        *n += 1;
        if *n == 1 {
            slug.to_string()
        } else {
            format!("{slug}-{}", *n - 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(md: &str) -> Rendered {
        render(md, &Options::default()).unwrap()
    }

    #[test]
    fn headings_title_toc_anchors() {
        let out = r("# Title\n\n## First\n\ntext\n\n### `sweep` <sup>fn</sup>\n\n## First\n");
        assert_eq!(out.title.as_deref(), Some("Title"));
        assert_eq!(out.toc.len(), 3);
        assert_eq!(out.toc[0].id, "first");
        assert_eq!(out.toc[2].id, "first-1"); // duplicate slug disambiguated
        // TOC text and ids are plain-text even when the heading has markup.
        assert_eq!(out.toc[1].text, "sweep fn");
        assert_eq!(out.toc[1].id, "sweep-fn");
        assert!(
            out.html
                .contains("<h2 id=\"first\">First<a class=\"anchor\"")
        );
    }

    #[test]
    fn paragraphs_and_inline() {
        let out = r("Some **bold**, *em*, `a<b` and a [link](x.md).\n");
        assert!(out.html.contains("<strong>bold</strong>"));
        assert!(out.html.contains("<em>em</em>"));
        assert!(out.html.contains("<code>a&lt;b</code>"));
        assert!(out.html.contains("<a href=\"x.md\">link</a>"));
    }

    #[test]
    fn link_rewriter_is_applied() {
        let opts = Options {
            link: &|h| h.replace(".md", "/"),
            code: &|_, _, _| None,
        };
        let out = render("[a](b.md)", &opts).unwrap();
        assert!(out.html.contains("href=\"b/\""));
    }

    #[test]
    fn link_destinations_keep_parens_and_drop_titles() {
        let out = r("[Big O](https://en.wikipedia.org/wiki/Big_O_notation_(complexity))\n");
        assert!(
            out.html
                .contains("<a href=\"https://en.wikipedia.org/wiki/Big_O_notation_(complexity)\">")
        );
        assert!(!out.html.contains("</a>)"));

        let titled = r("[Home](index.md \"Home page\")\n");
        assert!(titled.html.contains("<a href=\"index.md\">Home</a>"));
    }

    #[test]
    fn fences_highlight_and_hook_overrides() {
        let out = r("```rust\nfn x() {}\n```\n");
        assert!(out.html.contains("tok-kw\">fn</span>"));
        let opts = Options {
            link: &|h| h.to_string(),
            code: &|lang, tags, _| {
                (lang == "rust" && tags.iter().any(|t| t == "capture-output"))
                    .then(|| "<div class=custom></div>".into())
            },
        };
        let out = render("```rust capture-output\nx\n```\n", &opts).unwrap();
        assert!(out.html.contains("<div class=custom>"));
    }

    #[test]
    fn lists_nested_and_ordered() {
        let out = r("- a\n- b\n  - b1\n- c\n\n1. one\n2. two\n");
        assert!(out.html.contains("<ul>\n<li>a</li>"));
        assert!(out.html.contains("<li>b<ul>\n<li>b1</li>\n</ul>\n</li>"));
        assert!(out.html.contains("<ol>\n<li>one</li>"));
    }

    #[test]
    fn tables_with_alignment() {
        let out = r("| a | b |\n|---|---:|\n| 1 | 2 |\n");
        assert!(out.html.contains("<th>a</th>"));
        assert!(out.html.contains("<th style=\"text-align:right\">b</th>"));
        assert!(out.html.contains("<td style=\"text-align:right\">2</td>"));
    }

    #[test]
    fn admonition_blockquote_hr_comments() {
        let out = r(
            "!!! note \"Watch out\"\n    Indented body.\n\n> quoted\n\n---\n\n<!-- gone -->\nafter\n",
        );
        assert!(out.html.contains("adm adm-note"));
        assert!(
            out.html
                .contains("<span class=\"adm-kind\">note</span> · Watch out")
        );
        assert!(out.html.contains("<blockquote>\n<p>quoted</p>"));
        assert!(out.html.contains("<hr>"));
        assert!(!out.html.contains("gone"));
        assert!(out.html.contains("<p>after</p>"));
    }

    #[test]
    fn admonition_without_custom_title_shows_kind_alone() {
        // No custom title means the kind IS the whole label — it must not
        // be wrapped in `.adm-kind` (that span exists to de-emphasize a
        // kind tag relative to a *different*, more prominent custom title;
        // with no custom title there's nothing for it to defer to).
        let out = r("!!! tip\n    Body.\n");
        assert!(out.html.contains("adm-title\">tip</p>"));
        assert!(!out.html.contains("adm-kind"));
    }

    #[test]
    fn raw_html_passes_through_unchanged() {
        let out = r("<figure class=\"x\">\n<b>hi</b>\n</figure>\n\npara\n");
        assert!(
            out.html
                .contains("<figure class=\"x\">\n<b>hi</b>\n</figure>")
        );
        // Inline HTML inside a paragraph also passes through.
        let out = r("uses <kbd>K</kbd> keys, 2 < 3 stays escaped\n");
        assert!(out.html.contains("<kbd>K</kbd>"));
        assert!(out.html.contains("2 &lt; 3"));
    }

    #[test]
    fn style_and_script_blocks_survive_blank_lines() {
        let out =
            r("<style>\n.a {\n  color: red;\n}\n\n.b {\n  color: blue;\n}\n</style>\n\npara\n");
        assert!(
            out.html
                .contains(".a {\n  color: red;\n}\n\n.b {\n  color: blue;\n}")
        );
        assert!(out.html.contains("<p>para</p>"));
        // A blank line still ends an ordinary raw HTML block.
        let out = r("<div>\nfirst\n\nsecond\n</div>\n");
        assert!(!out.html.contains("second\n</div>"));
    }

    #[test]
    fn strict_errors_for_unsupported_syntax() {
        assert!(render("Heading\n=======\n", &Options::default()).is_err());
        assert!(render("[ref]: https://example.com\n", &Options::default()).is_err());
        assert!(render("```rust\nunclosed\n", &Options::default()).is_err());
    }

    #[test]
    fn content_tabs_render_as_radio_groups() {
        let out = r(
            "=== \"Library\"\n\n    ### Inside\n\n    body one\n\n=== \"CLI\"\n\n    ```bash\n    fq quote\n    ```\n\nafter tabs\n",
        );
        assert_eq!(out.html.matches("<section class=\"tab-panel\">").count(), 2);
        assert_eq!(out.html.matches("type=\"radio\"").count(), 2);
        assert!(out.html.contains("name=\"tabs-1\""));
        assert!(out.html.contains(">Library</label>"));
        assert!(out.html.contains("checked"));
        assert!(out.html.contains("<h3 id=\"inside\">")); // body is markdown
        assert!(out.html.contains("fq quote")); // fenced code inside a tab
        assert!(out.html.contains("<p>after tabs</p>"));
    }

    #[test]
    fn headings_inside_tab_panels_are_not_added_to_toc() {
        // Regression: every tab-panel body used to feed the same shared
        // heading collector as top-level content, so a page with N tabs
        // each containing "### Getting Started" produced N identical,
        // undifferentiated "Getting Started" entries in the on-page TOC —
        // none of them independently reachable, since only one panel is
        // visible at a time (CSS-only radio switching, no JS).
        let out = r(
            "## Top Level\n\n=== \"Library\"\n\n    ### Getting Started\n\n=== \"CLI\"\n\n    ### Getting Started\n\nafter tabs\n",
        );
        assert_eq!(out.toc.len(), 1);
        assert_eq!(out.toc[0].text, "Top Level");
        // The headings still render with disambiguated anchors — just not in the TOC.
        assert!(out.html.contains("<h3 id=\"getting-started\">"));
        assert!(out.html.contains("<h3 id=\"getting-started-1\">"));
    }

    #[test]
    fn images_and_autolinks() {
        let out = r("![alt](img.svg) and <https://a.b>\n");
        assert!(out.html.contains("<img src=\"img.svg\" alt=\"alt\">"));
        assert!(out.html.contains("<a href=\"https://a.b\">https://a.b</a>"));
    }

    #[test]
    fn multibyte_text_in_tags_and_autolinks_does_not_desync() {
        // `find('>')` returns byte offsets; the cursor is a char index. A
        // multibyte char before `>` must not drop the text that follows.
        let out = r("a <span title=\"café\">b</span> c ends\n");
        assert!(out.html.contains("</span> c ends"), "{}", out.html);
        let out = r("x <https://例え.jp/a> y ends\n");
        assert!(out.html.contains("y ends"), "{}", out.html);
        assert!(out.html.contains("https://例え.jp/a"));
    }

    #[test]
    fn block_html_interrupts_a_paragraph() {
        // A plugin-emitted chip directly after prose must not be absorbed
        // into the paragraph and double-escaped.
        let out = r("prose line\n<div class=\"claimline\">&lt;ok&gt;</div>\nmore prose\n");
        assert!(out.html.contains("<p>prose line</p>"), "{}", out.html);
        assert!(out.html.contains("&lt;ok&gt;"));
        assert!(
            !out.html.contains("&amp;lt;"),
            "double-escaped: {}",
            out.html
        );
        // Inline tags mid-paragraph still don't break it.
        let out = r("uses <kbd>K</kbd> keys\n");
        assert!(out.html.contains("<p>uses <kbd>K</kbd> keys</p>"));
    }
}
