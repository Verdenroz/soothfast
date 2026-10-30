use tree_sitter::Node;

use crate::lang::Lang;
use crate::scan::SymbolKind;

/// What a syntax node means to the symbol walk.
pub(crate) enum Decl {
    /// Not a declaration; look inside it for ones.
    Skip,
    /// Transparent syntax around a declaration (decorators, `export`). The
    /// declaration's span grows to cover the outermost wrapper.
    Wrap,
    /// A declaration. `scope` is true when its members are symbols too.
    Symbol {
        kind: SymbolKind,
        name: String,
        scope: bool,
    },
}

pub(crate) fn classify(lang: Lang, node: Node, src: &str) -> Decl {
    match lang {
        Lang::Python => python(node, src),
    }
}

fn text<'a>(node: Node, src: &'a str) -> &'a str {
    &src[node.byte_range()]
}

fn named(node: Node, src: &str, kind: SymbolKind, scope: bool) -> Decl {
    match node.child_by_field_name("name") {
        Some(name) => Decl::Symbol {
            kind,
            name: text(name, src).to_string(),
            scope,
        },
        None => Decl::Skip,
    }
}

fn python(node: Node, src: &str) -> Decl {
    match node.kind() {
        "decorated_definition" => Decl::Wrap,
        "function_definition" => named(node, src, SymbolKind::Function, false),
        "class_definition" => named(node, src, SymbolKind::Class, true),
        _ => Decl::Skip,
    }
}
