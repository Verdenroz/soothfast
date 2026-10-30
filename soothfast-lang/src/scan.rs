use std::collections::HashSet;

use tree_sitter::{Node, Tree};

use crate::classify::{Decl, classify};
use crate::lang::{Error, Lang};
use crate::visibility::{declared_public, exported_names, has_docstring};

/// The syntactic category of a [`Symbol`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    /// A free function.
    Function,
    /// A function or constructor declared inside a type.
    Method,
    /// A class, struct-like record or abstract class.
    Class,
    /// An interface.
    Interface,
    /// An enumeration.
    Enum,
    /// A singleton object (Kotlin `object`, `companion object`).
    Object,
    /// A type alias.
    TypeAlias,
}

/// A declaration found in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// Dotted path of the enclosing named containers plus the symbol's own
    /// name, such as `Outer.Inner.method`. Overloads share one name.
    pub name: String,
    /// What kind of declaration this is.
    pub kind: SymbolKind,
    /// First line of the span, 1-based, including decorators and modifiers.
    pub start_line: usize,
    /// Last line of the span, 1-based and inclusive.
    pub end_line: usize,
    /// Byte offset where the span starts.
    pub start_byte: usize,
    /// Byte offset one past the end of the span.
    pub end_byte: usize,
    /// Whether the symbol is part of the public surface: `public` in Java
    /// and Kotlin (Kotlin's default), `export`ed in JavaScript and
    /// TypeScript, no leading underscore in Python. Members count only when
    /// their container does.
    pub public: bool,
    /// Whether a doc comment sits directly above the declaration, or a
    /// Python docstring opens its body.
    pub documented: bool,
}

/// A symbol plus where its fingerprint text begins: a doc comment directly
/// above the declaration belongs to it.
pub(crate) struct Found {
    pub symbol: Symbol,
    pub text_start: usize,
}

/// Every declaration in `source`, in source order.
///
/// Syntax errors do not fail the call: declarations that parsed are still
/// returned. Only a file the parser cannot process at all is an [`Error`].
pub fn symbols(lang: Lang, source: &str) -> Result<Vec<Symbol>, Error> {
    Ok(scan(lang, source)?
        .1
        .into_iter()
        .map(|f| f.symbol)
        .collect())
}

/// The declarations of `source` that are part of its public surface.
pub fn public_symbols(lang: Lang, source: &str) -> Result<Vec<Symbol>, Error> {
    Ok(symbols(lang, source)?
        .into_iter()
        .filter(|s| s.public)
        .collect())
}

pub(crate) fn scan(lang: Lang, source: &str) -> Result<(Tree, Vec<Found>), Error> {
    let tree = lang.parse(source)?;
    let mut walk = Walk {
        lang,
        source,
        found: Vec::new(),
        exported: exported_names(tree.root_node(), source),
    };
    walk.visit(tree.root_node(), "", false, true, None);
    Ok((tree, walk.found))
}

struct Walk<'a> {
    lang: Lang,
    source: &'a str,
    found: Vec<Found>,
    exported: HashSet<String>,
}

impl Walk<'_> {
    fn visit(
        &mut self,
        node: Node,
        prefix: &str,
        in_type: bool,
        public: bool,
        outer: Option<Node>,
    ) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            // Recovered members lose their container, so their paths would be wrong.
            if child.is_error() {
                continue;
            }
            match classify(self.lang, child, self.source) {
                Decl::Skip => self.visit(child, prefix, in_type, public, None),
                Decl::Wrap => self.visit(child, prefix, in_type, public, outer.or(Some(child))),
                Decl::Scope(name) => self.visit(child, &join(prefix, &name), false, public, None),
                Decl::Symbol { kind, name, scope } => {
                    let path = join(prefix, &name);
                    let kind = match kind {
                        SymbolKind::Function if in_type => SymbolKind::Method,
                        other => other,
                    };
                    let public = public
                        && declared_public(
                            self.lang,
                            child,
                            &name,
                            self.source,
                            in_type,
                            outer,
                            &self.exported,
                        );
                    self.record(child, outer.unwrap_or(child), &path, kind, public);
                    if scope {
                        self.visit(child, &path, true, public, None);
                    }
                }
            }
        }
    }

    fn record(&mut self, decl: Node, span: Node, name: &str, kind: SymbolKind, public: bool) {
        let comment = doc_comment_above(span, self.source);
        let text_start = comment.map_or(span.start_byte(), |c| c.start_byte());
        let documented = match self.lang {
            Lang::Python => has_docstring(decl),
            _ => comment.is_some(),
        };
        self.found.push(Found {
            symbol: Symbol {
                name: name.to_string(),
                kind,
                start_line: span.start_position().row + 1,
                end_line: span.end_position().row + 1,
                start_byte: span.start_byte(),
                end_byte: span.end_byte(),
                public,
                documented,
            },
            text_start,
        });
    }
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

pub(crate) fn is_comment(node: Node) -> bool {
    node.kind().ends_with("comment")
}

pub(crate) fn is_doc_comment(node: Node, source: &str) -> bool {
    let text = &source[node.byte_range()];
    text.starts_with("/**") && !text.starts_with("/**/")
}

fn doc_comment_above<'t>(span: Node<'t>, source: &str) -> Option<Node<'t>> {
    span.prev_sibling()
        .filter(|c| is_comment(*c) && is_doc_comment(*c, source))
}
