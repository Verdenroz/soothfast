//! One type, in the single spelling every side of the magnus boundary uses.
//!
//! Ruby copies every buffer regardless of the signature (see the module
//! doc), so unlike JNI there is no second, narrower spelling to keep in
//! sync: whatever crosses is either a primitive magnus converts on its own,
//! or one of the two shapes [`glue`](super::glue) names explicitly.

use crate::model::Ty;

/// A Rust name, snake_cased for a Ruby method or symbol spelling.
pub(crate) fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let mut prev_lower = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            if prev_lower && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower = false;
        } else if c.is_ascii_alphanumeric() {
            prev_lower = true;
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
            prev_lower = false;
        }
    }
    out.trim_matches('_').to_string()
}

/// `PascalCase`s a hyphen- or underscore-separated name for the Ruby module
/// every class and the package `Error` are defined under.
pub(crate) fn module_name(name: &str) -> String {
    name.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(pascal)
        .collect()
}

fn pascal(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// A type as it appears in generated signatures, where an exported type is
/// its wrapper rather than the Rust type the wrapper holds, and every
/// primitive is spelled as itself: magnus converts each one without help.
/// [`super::glue`] overrides this for the two shapes that need one (a
/// mirrored enum, a buffer), so this is only ever reached for a type that
/// needs none.
pub(crate) fn signature_ty(ty: &Ty) -> String {
    let nest = signature_ty;
    match ty {
        Ty::Str => "String".into(),
        Ty::Bytes => "Vec<u8>".into(),
        Ty::List(inner) => format!("Vec<{}>", nest(inner)),
        Ty::Map(key, value) => format!(
            "::std::collections::HashMap<{}, {}>",
            nest(key),
            nest(value)
        ),
        Ty::Optional(inner) => format!("Option<{}>", nest(inner)),
        Ty::Tuple(items) => {
            let rendered: Vec<String> = items.iter().map(nest).collect();
            format!("({})", rendered.join(", "))
        }
        Ty::Class(name) => name.clone(),
        Ty::Opaque(path) => format!("::{path}"),
        primitive => primitive.render(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_names_join_pascalcased_words_across_either_separator() {
        assert_eq!(module_name("acme-core"), "AcmeCore");
        assert_eq!(module_name("acme_core"), "AcmeCore");
    }

    #[test]
    fn snake_lowercases_camel_and_pascal_boundaries() {
        assert_eq!(snake("Level"), "level");
        assert_eq!(snake("BumpAll"), "bump_all");
    }
}
