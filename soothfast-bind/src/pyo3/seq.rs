//! A `Vec<T>` field of exported handles, read as one `{T}Seq` handle.
//!
//! A getter that builds a handle per element costs a Python object per
//! element on every attribute access, which a loop indexing the field turns
//! quadratic. The seq clones the vector once and builds an element's handle
//! only when it is indexed; a column getter reads one field of every element
//! at once, through the same array classes a `Vec<f64>` return uses.

use std::collections::BTreeSet;
use std::fmt::Write;

use crate::model::Ty;
use crate::plan::{Accessor, BindingPlan, Class};

use super::buffers::{array_name, buffered};
use super::glue::{docs, inner_path};
use super::py_ident;

/// The class a field of `Vec<Class>` holds, when the class is a handle: a
/// mirrored enum's sequence stays a plain list.
pub(crate) fn element<'a>(ty: &'a Ty, plan: &BindingPlan) -> Option<&'a str> {
    match ty {
        Ty::List(inner) => match &**inner {
            Ty::Class(name) if !plan.is_mirrored(name) => Some(name),
            _ => None,
        },
        _ => None,
    }
}

/// Every class some field holds a sequence of, each once, in plan order.
pub(crate) fn classes(plan: &BindingPlan) -> Vec<&Class> {
    let wanted: BTreeSet<&str> = plan
        .classes
        .iter()
        .flat_map(|c| c.accessors.iter())
        .filter_map(|a| element(&a.ty, plan))
        .collect();
    plan.classes
        .iter()
        .filter(|c| wanted.contains(c.name.as_str()))
        .collect()
}

/// The sequence types every column getter hands back, so their array
/// classes get emitted alongside the ones returns and fields already need.
pub(crate) fn column_types(plan: &BindingPlan) -> Vec<Ty> {
    classes(plan)
        .into_iter()
        .flat_map(|c| c.accessors.iter())
        .filter(|a| buffered(&a.ty).is_some())
        .map(|a| Ty::List(Box::new(a.ty.clone())))
        .collect()
}

pub(crate) fn name(class: &str) -> String {
    format!("{class}Seq")
}

/// The field getter: one clone of the vector into the seq.
pub(crate) fn getter(accessor: &Accessor, class: &str) -> String {
    let field = &accessor.field;
    format!(
        "{}    #[getter]\n    fn {}(&self) -> {} {{\n        {}::new(self.0.{field}.clone())\n    }}\n",
        docs(accessor.doc.as_deref(), "    "),
        py_ident(field),
        name(class),
        name(class),
    )
}

/// One seq class over the element class's own Rust type.
pub(crate) fn render(class: &Class, krate: &str) -> String {
    let seq = name(&class.name);
    let element = &class.name;
    let inner = inner_path(&class.rust_path, krate);
    let columns: String = class.accessors.iter().filter_map(column).collect();
    format!(
        "
/// A sequence of `{element}` read from a field. Indexing builds one handle;
/// `x.tolist()` builds them all; a column getter reads one field of every
/// element at once.
#[pyclass(name = \"{seq}\")]
pub struct {seq}(Vec<{inner}>);

impl {seq} {{
    fn new(values: Vec<{inner}>) -> Self {{
        {seq}(values)
    }}
}}

#[pymethods]
impl {seq} {{
    fn __len__(&self) -> usize {{
        self.0.len()
    }}

    fn __getitem__(&self, index: isize) -> ::pyo3::PyResult<{element}> {{
        let at = match index < 0 {{
            true => index + self.0.len() as isize,
            false => index,
        }};
        usize::try_from(at)
            .ok()
            .and_then(|at| self.0.get(at))
            .cloned()
            .map({element})
            .ok_or_else(|| ::pyo3::exceptions::PyIndexError::new_err(\"index out of range\"))
    }}

    fn __iter__<'py>(
        &self,
        py: ::pyo3::Python<'py>,
    ) -> ::pyo3::PyResult<::pyo3::Bound<'py, ::pyo3::PyAny>> {{
        use ::pyo3::types::PyAnyMethods;
        Ok(::pyo3::types::PyList::new(py, self.tolist())?.try_iter()?.into_any())
    }}

    fn __repr__(&self) -> String {{
        format!(\"{seq}(len={{}})\", self.0.len())
    }}

    /// Every element as a handle, in a plain list.
    fn tolist(&self) -> Vec<{element}> {{
        self.0.iter().cloned().map({element}).collect()
    }}
{columns}}}
"
    )
}

/// A column getter for a field every element carries: a buffered primitive
/// comes back as its array class, an optional one or a string as a list.
fn column(accessor: &Accessor) -> Option<String> {
    let field = &accessor.field;
    let (ty, read) = match &accessor.ty {
        ty if buffered(ty).is_some() => {
            let array = array_name(&Ty::List(Box::new(ty.clone())))?;
            (
                array.clone(),
                format!("{array}::new(self.0.iter().map(|v| v.{field}).collect())"),
            )
        }
        Ty::Optional(inner) => {
            let element = buffered(inner)?;
            (
                format!("Vec<Option<{element}>>"),
                format!("self.0.iter().map(|v| v.{field}).collect()"),
            )
        }
        Ty::Str => (
            "Vec<String>".into(),
            format!("self.0.iter().map(|v| v.{field}.clone()).collect()"),
        ),
        _ => return None,
    };
    let mut out = String::new();
    let _ = write!(
        out,
        "\n{}    #[getter]\n    fn {}(&self) -> {ty} {{\n        {read}\n    }}\n",
        docs(accessor.doc.as_deref(), "    "),
        py_ident(field),
    );
    Some(out)
}
