//! `__repr__` for a handle class: its readable fields, in accessor order,
//! as `Name(field=value, ...)`. Only a field the glue can show without
//! anything from the bound crate is listed: a `Debug` value, a mirrored
//! enum by its Python name (optional or not), or a mapped type through
//! `Display`.

use crate::model::Ty;
use crate::plan::{Accessor, BindingPlan, Class};

use super::glue::slot_name;
use super::py_ident;

/// The member, or `None` for a class with nothing to show, which keeps
/// pyo3's own repr. A consumable class always gets one, even with nothing
/// else to show: it is the only place a consumed handle prints who
/// consumed it.
pub(crate) fn member(class: &Class, plan: &BindingPlan) -> Option<String> {
    let consumable = plan.is_consumable(&class.name);
    let base = if consumable { "inner" } else { "self.0" };
    let shown: Vec<(String, String)> = class
        .accessors
        .iter()
        .filter_map(|a| slot(a, plan, base))
        .collect();
    if shown.is_empty() && !consumable {
        return None;
    }
    let (slots, args): (Vec<String>, Vec<String>) = shown.into_iter().unzip();
    let live = format!(
        "format!(\"{}({})\", {})",
        class.name,
        slots.join(", "),
        args.join(", "),
    );
    if !consumable {
        return Some(format!(
            "    fn __repr__(&self) -> String {{\n        {live}\n    }}\n"
        ));
    }
    let slot = slot_name(&class.name);
    Some(format!(
        "    fn __repr__(&self) -> String {{\n        let guard = self.0.lock().unwrap_or_else(|e| e.into_inner());\n        match &*guard {{\n            {slot}::Live(inner) => {live},\n            {slot}::Consumed(by) => format!(\"{}(<consumed by {{by}}>)\"),\n        }}\n    }}\n",
        class.name,
    ))
}

/// One `name=slot` piece of the format string and the value filling it.
fn slot(accessor: &Accessor, plan: &BindingPlan, base: &str) -> Option<(String, String)> {
    let name = py_ident(&accessor.field);
    let value = format!("{base}.{}", accessor.field);
    Some(match &accessor.ty {
        Ty::Class(class) if plan.is_mirrored(class) => (
            format!("{name}={class}.{{:?}}"),
            format!("{class}::from(&{value})"),
        ),
        Ty::Optional(inner) if matches!(&**inner, Ty::Class(c) if plan.is_mirrored(c)) => {
            let Ty::Class(class) = &**inner else {
                return None;
            };
            (
                format!("{name}={{}}"),
                format!(
                    "match {value}.as_ref() {{ Some(v) => format!(\"Some({class}.{{:?}})\", {class}::from(v)), None => String::from(\"None\") }}"
                ),
            )
        }
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
