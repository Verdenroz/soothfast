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
        .map(|f| normalized(source, f.text_start, f.symbol.end_byte, &ordinary))
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
/// beside it can't fuse.
fn normalized(source: &str, start: usize, end: usize, ordinary: &[(usize, usize)]) -> String {
    let mut text = String::new();
    let mut at = start;
    for &(from, to) in ordinary.iter().filter(|&&(f, t)| f >= start && t <= end) {
        text.push_str(&source[at..from]);
        text.push(' ');
        at = to;
    }
    text.push_str(&source[at..end]);
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
