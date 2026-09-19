//! The error classes a language raises: one per thrown type, and one per
//! variant of an enum type, carrying the variant's own fields as attributes.

use crate::model::{ErrorType, Ty, VariantFields};

/// One thrown error type, as the exception class a language raises for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorClass {
    pub name: String,
    pub rust_path: String,
    pub doc: Option<String>,
    /// Empty for a struct error type.
    pub variants: Vec<ErrorVariant>,
}

/// One variant of an enum error type, as a subclass of its type's class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorVariant {
    pub name: String,
    pub doc: Option<String>,
    /// The named fields a raised error carries as attributes: primitives,
    /// strings, bytes, and options of those. Anything else stays in the
    /// message only.
    pub fields: Vec<(String, Ty)>,
    pub shape: VariantShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariantShape {
    Unit,
    Tuple,
    Named,
}

pub(super) fn lower(errors: &[ErrorType]) -> Vec<ErrorClass> {
    errors
        .iter()
        .map(|e| ErrorClass {
            name: e.name.clone(),
            rust_path: e.rust_path.clone(),
            doc: e.doc.clone(),
            variants: e
                .variants
                .iter()
                .flatten()
                .map(|v| ErrorVariant {
                    name: v.name.clone(),
                    doc: v.doc.clone(),
                    fields: attributes(&v.fields),
                    shape: match &v.fields {
                        VariantFields::Unit => VariantShape::Unit,
                        VariantFields::Tuple(_) => VariantShape::Tuple,
                        VariantFields::Named(_) => VariantShape::Named,
                    },
                })
                .collect(),
        })
        .collect()
}

fn attributes(fields: &VariantFields) -> Vec<(String, Ty)> {
    match fields {
        VariantFields::Named(fields) => fields
            .iter()
            .filter(|f| carried(&f.ty))
            .map(|f| (f.name.clone(), f.ty.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

fn carried(ty: &Ty) -> bool {
    match ty {
        Ty::Str | Ty::Bytes => true,
        Ty::Optional(inner) => carried(inner),
        other => other.is_primitive(),
    }
}
