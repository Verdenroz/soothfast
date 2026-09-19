//! `__repr__` for a handle class: its readable fields, in accessor order,
//! as `Name(field=value, ...)`. Only a field the glue can show without
//! anything from the bound crate is listed: a `Debug` value, a mirrored
//! enum by its Python name, or a mapped type through `Display`.

use crate::model::Ty;
use crate::plan::{Accessor, BindingPlan, Class};

use super::py_ident;

/// The member, or `None` for a class with nothing to show, which keeps
/// pyo3's own repr.
pub(crate) fn member(class: &Class, plan: &BindingPlan) -> Option<String> {
    let shown: Vec<(String, String)> = class
        .accessors
        .iter()
        .filter_map(|a| slot(a, plan))
        .collect();
    if shown.is_empty() {
        return None;
    }
    let (slots, args): (Vec<String>, Vec<String>) = shown.into_iter().unzip();
    Some(format!(
        "    fn __repr__(&self) -> String {{\n        format!(\"{}({})\", {})\n    }}\n",
        class.name,
        slots.join(", "),
        args.join(", "),
    ))
}

/// One `name=slot` piece of the format string and the value filling it.
fn slot(accessor: &Accessor, plan: &BindingPlan) -> Option<(String, String)> {
    let name = py_ident(&accessor.field);
    let value = format!("self.0.{}", accessor.field);
    Some(match &accessor.ty {
        Ty::Class(class) if plan.is_mirrored(class) => (
            format!("{name}={class}.{{:?}}"),
            format!("{class}::from(&{value})"),
        ),
        Ty::Text(_) => (
            format!("{name}={{:?}}"),
            format!("::std::string::ToString::to_string(&{value})"),
        ),
        ty if debuggable(ty) => (format!("{name}={{:?}}"), value),
        _ => return None,
    })
}

/// Whether `{:?}` of the value needs nothing from the bound crate.
fn debuggable(ty: &Ty) -> bool {
    match ty {
        Ty::Str | Ty::Bytes => true,
        Ty::Optional(inner) | Ty::List(inner) => debuggable(inner),
        other => other.is_primitive(),
    }
}
