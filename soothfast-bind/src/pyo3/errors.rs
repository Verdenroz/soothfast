//! The package's exception hierarchy: an `Error` base, one subclass per
//! thrown type, and one per variant of an enum type with the variant's own
//! fields set as attributes, so a caller can `except` a case rather than
//! parse a message.

use std::collections::BTreeSet;
use std::fmt::Write;

use crate::BindOptions;
use crate::model::Ty;
use crate::plan::{BindingPlan, ErrorClass, ErrorVariant, VariantShape};

use super::buffers;
use super::glue::inner_path;
use super::{py_ident, seq};

/// Every exception class's Python name, decided once so the declarations,
/// the raising code and the module registration agree.
pub(crate) struct Hierarchy<'a> {
    base: String,
    types: Vec<Named<'a>>,
}

struct Named<'a> {
    class: &'a ErrorClass,
    name: String,
    variants: Vec<(&'a ErrorVariant, String)>,
}

/// Name every class. A name already taken by a class, seq, array, function
/// or earlier error class takes an `Error` suffix; a second collision is an
/// error, since guessing further would only hide it.
pub(crate) fn hierarchy<'a>(
    plan: &'a BindingPlan,
    opts: &BindOptions,
) -> Result<Hierarchy<'a>, String> {
    let mut taken: BTreeSet<String> = plan.classes.iter().map(|c| c.name.clone()).collect();
    taken.extend(seq::classes(plan).into_iter().map(|c| seq::name(&c.name)));
    taken.extend(buffers::arrays(plan).into_iter().map(|(name, _)| name));
    taken.extend(plan.functions.iter().map(|f| py_ident(&f.name)));

    let base = claim("Error", &mut taken, &opts.package)?;
    let mut types = Vec::new();
    for class in &plan.errors {
        let name = claim(&class.name, &mut taken, &opts.package)?;
        let mut variants = Vec::new();
        for variant in &class.variants {
            variants.push((variant, claim(&variant.name, &mut taken, &opts.package)?));
        }
        types.push(Named {
            class,
            name,
            variants,
        });
    }
    Ok(Hierarchy { base, types })
}

fn claim(wanted: &str, taken: &mut BTreeSet<String>, package: &str) -> Result<String, String> {
    let name = match taken.contains(wanted) {
        false => wanted.to_string(),
        true => format!("{wanted}Error"),
    };
    if !taken.insert(name.clone()) {
        return Err(format!(
            "{package}: error class `{wanted}` collides with another name and so does `{name}`"
        ));
    }
    Ok(name)
}

impl Hierarchy<'_> {
    /// The `create_exception!` declarations, base first, each type before
    /// its variants.
    pub(crate) fn declarations(&self, opts: &BindOptions) -> String {
        let module = &opts.module;
        let mut out = format!(
            "\n::pyo3::create_exception!({module}, {}, ::pyo3::exceptions::PyException, \"Base of every error {} raises.\");\n",
            self.base, opts.package
        );
        for ty in &self.types {
            let doc = ty
                .class
                .doc
                .clone()
                .unwrap_or_else(|| format!("A `{}` error.", ty.class.rust_path));
            let _ = writeln!(
                out,
                "::pyo3::create_exception!({module}, {}, {}, \"{}\");",
                ty.name,
                self.base,
                quoted(&doc)
            );
            for (variant, name) in &ty.variants {
                let doc = variant
                    .doc
                    .clone()
                    .unwrap_or_else(|| format!("`{}::{}`.", ty.class.name, variant.name));
                let _ = writeln!(
                    out,
                    "::pyo3::create_exception!({module}, {name}, {}, \"{}\");",
                    ty.name,
                    quoted(&doc)
                );
            }
        }
        out
    }

    /// One `m.add` per class, in declaration order.
    pub(crate) fn registrations(&self) -> String {
        let mut out = String::new();
        let mut add = |name: &str| {
            let _ = writeln!(out, "    m.add(\"{name}\", m.py().get_type::<{name}>())?;");
        };
        add(&self.base);
        for ty in &self.types {
            add(&ty.name);
            for (_, name) in &ty.variants {
                add(name);
            }
        }
        out
    }

    /// The body of `From<Newtype> for PyErr`, over `err.0` with `message`
    /// already bound: the variant's class for an enum, the type's class for
    /// a struct, the base for a bare `String`.
    pub(crate) fn raise(&self, thrown: &Ty, krate: &str) -> String {
        let Some(ty) = self.class_for(thrown) else {
            return format!("        {}::new_err(message)\n", self.base);
        };
        if ty.variants.is_empty() {
            return format!("        {}::new_err(message)\n", ty.name);
        }
        let path = inner_path(&ty.class.rust_path, krate);
        let mut out = String::from("        match err.0 {\n");
        for (variant, name) in &ty.variants {
            out.push_str(&arm(variant, name, &path));
        }
        let _ = write!(
            out,
            "            _ => {}::new_err(message),\n        }}\n",
            ty.name
        );
        out
    }

    /// Whether raising this type matches on variants, and so carries the
    /// catch-all arm a local exhaustive enum would warn about.
    pub(crate) fn matches(&self, thrown: &Ty) -> bool {
        self.class_for(thrown)
            .is_some_and(|t| !t.variants.is_empty())
    }

    /// The thrown type as the glue spells it, by the public path the walk
    /// found rather than the canonical one a private module may sit on.
    pub(crate) fn spelling(&self, thrown: &Ty, krate: &str) -> Option<String> {
        self.class_for(thrown)
            .map(|t| inner_path(&t.class.rust_path, krate))
    }

    /// The class for a thrown type: the exported class's own name, or an
    /// unexported type's path, which the walk spelled the same way.
    fn class_for(&self, thrown: &Ty) -> Option<&Named<'_>> {
        let (path, name) = match thrown {
            Ty::Class(name) => (None, name.as_str()),
            Ty::Opaque(path) => (Some(path.as_str()), path.rsplit("::").next()?),
            _ => return None,
        };
        self.types
            .iter()
            .find(|t| path == Some(t.class.rust_path.as_str()))
            .or_else(|| self.types.iter().find(|t| t.class.name == name))
    }
}

/// One match arm. A named variant is always matched with `..`: a
/// `non_exhaustive` variant from another crate demands it, and it is harmless
/// on one that is not. Attribute values move out of the matched error.
fn arm(variant: &ErrorVariant, name: &str, path: &str) -> String {
    let rust = &variant.name;
    match variant.shape {
        VariantShape::Unit => format!("            {path}::{rust} => {name}::new_err(message),\n"),
        VariantShape::Tuple => {
            format!("            {path}::{rust}(..) => {name}::new_err(message),\n")
        }
        VariantShape::Named if variant.fields.is_empty() => {
            format!("            {path}::{rust} {{ .. }} => {name}::new_err(message),\n")
        }
        VariantShape::Named => {
            let bound: Vec<&str> = variant.fields.iter().map(|(f, _)| f.as_str()).collect();
            let sets: String = variant
                .fields
                .iter()
                .map(|(field, _)| {
                    format!(
                        "                    let _ = value.setattr(\"{}\", {field});\n",
                        py_ident(field)
                    )
                })
                .collect();
            format!(
                "            {path}::{rust} {{ {}, .. }} => {{\n                let e = {name}::new_err(message);\n                ::pyo3::Python::attach(|py| {{\n                    let value = e.value(py);\n{sets}                }});\n                e\n            }}\n",
                bound.join(", ")
            )
        }
    }
}

/// A doc summary as a Rust string literal's contents.
fn quoted(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}
