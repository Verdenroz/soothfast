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
    /// A named container that is not a symbol itself (a TypeScript
    /// namespace); its members are named under it.
    Scope(String),
    /// A declaration. `scope` is true when its members are symbols too.
    Symbol {
        kind: SymbolKind,
        name: String,
        scope: bool,
    },
}

pub(crate) fn classify(lang: Lang, node: Node, src: &str) -> Decl {
    match lang {
        Lang::Java => java(node, src),
        Lang::Kotlin => kotlin(node, src),
        Lang::Python => python(node, src),
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => ecma(node, src),
    }
}

fn text<'a>(node: Node, src: &'a str) -> &'a str {
    &src[node.byte_range()]
}

fn named_by(node: Node, field: &str, src: &str, kind: SymbolKind, scope: bool) -> Decl {
    match node.child_by_field_name(field) {
        Some(name) => Decl::Symbol {
            kind,
            name: text(name, src).to_string(),
            scope,
        },
        None => Decl::Skip,
    }
}

fn named(node: Node, src: &str, kind: SymbolKind, scope: bool) -> Decl {
    named_by(node, "name", src, kind, scope)
}

fn parent_kind<'t>(node: Node<'t>) -> &'t str {
    node.parent().map_or("", |p| p.kind())
}

fn java(node: Node, src: &str) -> Decl {
    match node.kind() {
        "class_declaration" | "record_declaration" => named(node, src, SymbolKind::Class, true),
        "interface_declaration" => named(node, src, SymbolKind::Interface, true),
        "enum_declaration" => named(node, src, SymbolKind::Enum, true),
        "method_declaration" | "constructor_declaration" | "compact_constructor_declaration" => {
            named(node, src, SymbolKind::Function, false)
        }
        _ => Decl::Skip,
    }
}

fn kotlin(node: Node, src: &str) -> Decl {
    match node.kind() {
        "class_declaration" => {
            let kind = kotlin_class_kind(node, src);
            named(node, src, kind, true)
        }
        "object_declaration" => named(node, src, SymbolKind::Object, true),
        "companion_object" => Decl::Symbol {
            kind: SymbolKind::Object,
            name: node
                .child_by_field_name("name")
                .map_or("Companion", |n| text(n, src))
                .to_string(),
            scope: true,
        },
        "function_declaration" => named(node, src, SymbolKind::Function, false),
        "type_alias" => named_by(node, "type", src, SymbolKind::TypeAlias, false),
        _ => Decl::Skip,
    }
}

fn kotlin_class_kind(node: Node, src: &str) -> SymbolKind {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "interface" => return SymbolKind::Interface,
            "modifiers" if text(child, src).split_whitespace().any(|m| m == "enum") => {
                return SymbolKind::Enum;
            }
            _ => {}
        }
    }
    SymbolKind::Class
}

fn python(node: Node, src: &str) -> Decl {
    match node.kind() {
        "decorated_definition" => Decl::Wrap,
        "function_definition" => named(node, src, SymbolKind::Function, false),
        "class_definition" => named(node, src, SymbolKind::Class, true),
        _ => Decl::Skip,
    }
}

const FUNCTION_VALUES: [&str; 4] = [
    "arrow_function",
    "function_expression",
    "function",
    "generator_function",
];

fn ecma(node: Node, src: &str) -> Decl {
    match node.kind() {
        "export_statement" | "ambient_declaration" => Decl::Wrap,
        "lexical_declaration" | "variable_declaration" if top_level(node) => Decl::Wrap,
        "variable_declarator" if top_level_declarator(node) => {
            let function_valued = node
                .child_by_field_name("value")
                .is_some_and(|v| FUNCTION_VALUES.contains(&v.kind()));
            if function_valued {
                named(node, src, SymbolKind::Function, false)
            } else {
                Decl::Skip
            }
        }
        "function_declaration" | "generator_function_declaration" | "function_signature" => {
            named(node, src, SymbolKind::Function, false)
        }
        "method_definition" | "abstract_method_signature" => {
            named(node, src, SymbolKind::Function, false)
        }
        "method_signature" if parent_kind(node) == "class_body" => {
            named(node, src, SymbolKind::Function, false)
        }
        "class_declaration" | "abstract_class_declaration" => {
            named(node, src, SymbolKind::Class, true)
        }
        "interface_declaration" => named(node, src, SymbolKind::Interface, false),
        "type_alias_declaration" => named(node, src, SymbolKind::TypeAlias, false),
        "enum_declaration" => named(node, src, SymbolKind::Enum, false),
        "internal_module" | "module" => match node.child_by_field_name("name") {
            Some(name) => Decl::Scope(text(name, src).to_string()),
            None => Decl::Skip,
        },
        _ => Decl::Skip,
    }
}

fn top_level(declaration: Node) -> bool {
    matches!(parent_kind(declaration), "program" | "export_statement")
}

fn top_level_declarator(declarator: Node) -> bool {
    declarator.parent().is_some_and(top_level)
}
