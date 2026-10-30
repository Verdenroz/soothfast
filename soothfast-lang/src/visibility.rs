use std::collections::HashSet;

use tree_sitter::Node;

use crate::lang::Lang;

/// Whether a declaration is public in its own right, ignoring its container.
/// `outer` is the outermost wrapper around it (`export`, decorators).
pub(crate) fn declared_public(
    lang: Lang,
    node: Node,
    name: &str,
    src: &str,
    in_type: bool,
    outer: Option<Node>,
    exported: &HashSet<String>,
) -> bool {
    let restricted = |kinds: &[&str]| keywords(node).iter().any(|k| kinds.contains(k));
    match lang {
        Lang::Java => {
            let keywords = keywords(node);
            if keywords
                .iter()
                .any(|k| matches!(*k, "public" | "private" | "protected"))
            {
                keywords.contains(&"public")
            } else {
                // Interface members are public without a modifier.
                node.parent()
                    .is_some_and(|p| matches!(p.kind(), "interface_body" | "annotation_type_body"))
            }
        }
        Lang::Kotlin => !restricted(&["private", "internal", "protected"]),
        Lang::Python => !name.starts_with('_'),
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx if in_type => {
            !name.starts_with('#') && !restricted_member(node, src)
        }
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => {
            outer.is_some_and(|o| o.kind() == "export_statement") || exported.contains(name)
        }
    }
}

/// Local names a JavaScript or TypeScript module exports without a modifier on
/// the declaration: `export { a, b as c }` and `export default a`. A
/// re-export from another module names nothing declared here.
pub(crate) fn exported_names(root: Node, src: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    let mut cursor = root.walk();
    for statement in root
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "export_statement" && n.child_by_field_name("source").is_none())
    {
        if let Some(value) = statement
            .child_by_field_name("value")
            .filter(|v| v.kind() == "identifier")
        {
            names.insert(src[value.byte_range()].to_string());
        }
        let mut inner = statement.walk();
        for clause in statement
            .named_children(&mut inner)
            .filter(|n| n.kind() == "export_clause")
        {
            let mut specs = clause.walk();
            for spec in clause.named_children(&mut specs) {
                if let Some(name) = spec.child_by_field_name("name") {
                    names.insert(src[name.byte_range()].to_string());
                }
            }
        }
    }
    names
}

/// Docstring of a Python function or class: a leading string statement.
pub(crate) fn has_docstring(node: Node) -> bool {
    node.child_by_field_name("body")
        .and_then(|body| body.named_child(0))
        .is_some_and(|first| {
            first.kind() == "expression_statement"
                && first.named_child(0).is_some_and(|e| e.kind() == "string")
        })
}

/// Modifier keywords written on a Java or Kotlin declaration. Kotlin wraps
/// its visibility keyword in a `visibility_modifier` node.
fn keywords(node: Node) -> Vec<&'static str> {
    let mut found = Vec::new();
    let mut cursor = node.walk();
    for modifiers in node
        .children(&mut cursor)
        .filter(|c| c.kind() == "modifiers")
    {
        let mut inner = modifiers.walk();
        for m in modifiers.children(&mut inner) {
            if m.kind() == "visibility_modifier" {
                found.extend(m.child(0).map(|k| k.kind()));
            } else if !m.is_named() {
                found.push(m.kind());
            }
        }
    }
    found
}

fn restricted_member(node: Node, src: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|c| {
        c.kind() == "accessibility_modifier"
            && matches!(&src[c.byte_range()], "private" | "protected")
    })
}
