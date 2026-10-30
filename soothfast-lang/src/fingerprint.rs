use tree_sitter::Node;

use crate::lang::Lang;
use crate::scan::{is_comment, is_doc_comment, scan};

/// Fingerprint of every span declared as `name`, or `None` when no symbol
/// has that name.
///
/// Mirrors the rustdoc fingerprint: the span text with ordinary comments
/// removed, doc comments kept (a doc comment directly above a declaration is
/// part of its span), whitespace collapsed, then FNV-1a. Overloads are
/// joined in source order. Returns 16 lowercase hex digits.
pub fn fingerprint(lang: Lang, source: &str, name: &str) -> Option<String> {
    let (tree, found) = scan(lang, source).ok()?;
    let mut ordinary = Vec::new();
    collect_ordinary_comments(tree.root_node(), source, &mut ordinary);

    let spans: Vec<String> = found
        .iter()
        .filter(|f| f.symbol.name == name)
        .map(|f| normalized(lang, source, f.text_start, f.symbol.end_byte, &ordinary))
        .collect();
    if spans.is_empty() {
        return None;
    }
    let hash = soothfast_registry::fnv1a(spans.join("\n").as_bytes());
    Some(format!("{hash:016x}"))
}

fn collect_ordinary_comments(node: Node, source: &str, out: &mut Vec<(usize, usize)>) {
    if is_comment(node) && !is_doc_comment(node, source) {
        out.push((node.start_byte(), node.end_byte()));
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_ordinary_comments(child, source, out);
    }
}

/// Span text with each ordinary comment replaced by a space so the tokens
/// beside it can't fuse. Whitespace collapses fully, except where indentation
/// is syntax: there each non-blank line keeps its depth relative to the
/// span's own line.
fn normalized(
    lang: Lang,
    source: &str,
    start: usize,
    end: usize,
    ordinary: &[(usize, usize)],
) -> String {
    let mut text = String::new();
    let mut at = start;
    for &(from, to) in ordinary.iter().filter(|&&(f, t)| f >= start && t <= end) {
        text.push_str(&source[at..from]);
        text.push(' ');
        at = to;
    }
    text.push_str(&source[at..end]);
    if !matches!(lang, Lang::Python) {
        return collapse(&text);
    }
    let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
    let base = indent_width(&source[line_start..]);
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(n, line)| {
            let depth = if n == 0 {
                0
            } else {
                indent_width(line).saturating_sub(base)
            };
            format!("{}{}", " ".repeat(depth), collapse(line))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Width of a line's leading whitespace, tabs advancing to multiples of 8.
fn indent_width(line: &str) -> usize {
    let mut width = 0;
    for c in line.chars() {
        match c {
            ' ' => width += 1,
            '\t' => width = (width / 8 + 1) * 8,
            _ => break,
        }
    }
    width
}
