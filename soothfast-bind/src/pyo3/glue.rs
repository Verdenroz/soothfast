//! The glue crate's `src/lib.rs`.
//!
//! Every exported type becomes a locally defined wrapper: `#[pyclass]`
//! expands to trait impls the orphan rule only permits in the crate that
//! defines the type. The same rule is why a failing call raises through a
//! local error newtype rather than `impl From<UserError> for PyErr`.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::model::{Ownership, Param, Receiver, Ty};
use crate::naming::ident_part;
use crate::plan::{Accessor, BindingPlan, Class, Function, Transfer, offloadable};
use crate::{BindOptions, GENERATED_RS, GLUE_ALLOW};

use super::asyncrt;
use super::buffers::{self, array_name, buffered, view_name};
use super::py_ident;

/// Every call with a writable buffer parameter alongside another buffer
/// parameter needs this: two arguments can name the same Python object
/// (`scale_into(a, 2.0, a)`), which would hand the Rust call a `&mut` and
/// a `&` (or two `&mut`s) over the same memory.
const ALIAS_CHECK: &str = "
fn buffers_alias(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1 && b.0 < a.1
}
";

/// Render the glue crate body.
pub(crate) fn render(plan: &BindingPlan, opts: &BindOptions) -> String {
    let krate = format!("::{}", opts.crate_name);
    let mut out = String::from(GENERATED_RS);
    out.push_str(GLUE_ALLOW);
    out.push_str("\nuse pyo3::prelude::*;\n");
    out.push_str(&asyncrt::preamble(plan));
    if needs_alias_check(plan) {
        out.push_str(ALIAS_CHECK);
    }

    for view in buffers::views(plan) {
        out.push_str(&view);
    }
    for (name, element) in buffers::arrays(plan) {
        out.push_str(&buffers::array(&name, &element));
    }
    for (ty, name) in error_newtypes(plan) {
        out.push_str(&error_impl(&ty, &name, &krate));
    }
    for class in &plan.classes {
        out.push_str(&class_block(class, &krate, plan));
    }
    for function in &plan.functions {
        out.push_str(&free_fn(function, &krate, plan));
    }
    out.push_str(&module_block(plan, opts));
    out
}

/// Whether any function in the surface has a writable buffer parameter
/// alongside another buffer parameter, the only shape [`ALIAS_CHECK`] guards.
fn needs_alias_check(plan: &BindingPlan) -> bool {
    plan.functions()
        .any(|f| !aliasing_pairs(f, plan).is_empty())
}

/// Every pair of buffer parameters that need a byte-range check against each
/// other: at least one of the two has to be writable, since two shared views
/// over the same memory alias safely.
fn aliasing_pairs(function: &Function, plan: &BindingPlan) -> Vec<(String, bool, String, bool)> {
    let buffers: Vec<(String, bool)> = function
        .params
        .iter()
        .filter_map(|p| match Transfer::of(p, plan) {
            Transfer::Buffer {
                element, writable, ..
            } => {
                let element: Ty = element.into();
                buffered(&element).map(|_| (py_ident(&p.name), writable))
            }
            _ => None,
        })
        .collect();
    let mut pairs = Vec::new();
    for i in 0..buffers.len() {
        for j in (i + 1)..buffers.len() {
            let (a_name, a_writable) = &buffers[i];
            let (b_name, b_writable) = &buffers[j];
            if *a_writable || *b_writable {
                pairs.push((a_name.clone(), *a_writable, b_name.clone(), *b_writable));
            }
        }
    }
    pairs
}

/// One `if` per pair [`aliasing_pairs`] names, raising `ValueError` on
/// overlap rather than letting the call alias two Rust references.
fn aliasing_check(function: &Function, plan: &BindingPlan, indent: &str) -> String {
    let mut out = String::new();
    for (a_name, a_writable, b_name, b_writable) in aliasing_pairs(function, plan) {
        let (writable_name, other_name, other_writable) = match a_writable {
            true => (&a_name, &b_name, b_writable),
            false => (&b_name, &a_name, a_writable),
        };
        let condition = match other_writable {
            true => {
                format!("buffers_alias({writable_name}.byte_range(), {other_name}.byte_range())")
            }
            false => format!(
                "{other_name}.byte_range().is_some_and(|r| buffers_alias({writable_name}.byte_range(), r))"
            ),
        };
        let _ = writeln!(
            out,
            "{indent}if {condition} {{\n{indent}    return Err(::pyo3::exceptions::PyValueError::new_err(\"{a_name} and {b_name} alias the same buffer\"));\n{indent}}}"
        );
    }
    out
}

/// Whether the generated signature has to return `PyResult`: the call
/// itself can fail, or an aliasing check ahead of it can.
fn fallible(function: &Function, plan: &BindingPlan) -> bool {
    function.throws.is_some() || !aliasing_pairs(function, plan).is_empty()
}

/// One newtype per distinct error type, named after it so several errors in
/// one package stay tellable apart.
fn error_newtypes(plan: &BindingPlan) -> Vec<(Ty, String)> {
    let mut seen: BTreeMap<String, Ty> = BTreeMap::new();
    for function in plan.functions() {
        if let Some(err) = &function.throws {
            seen.insert(err.render(), err.clone());
        }
    }
    seen.into_values()
        .map(|ty| {
            let name = error_name(&ty);
            (ty, name)
        })
        .collect()
}

fn error_name(ty: &Ty) -> String {
    format!("BindError{}", ident_part(&ty.render()))
}

fn error_impl(ty: &Ty, name: &str, krate: &str) -> String {
    let inner = error_ty(ty, krate);
    format!(
        "
struct {name}({inner});

impl ::std::convert::From<{name}> for ::pyo3::PyErr {{
    fn from(err: {name}) -> ::pyo3::PyErr {{
        ::pyo3::exceptions::PyRuntimeError::new_err(::std::string::ToString::to_string(&err.0))
    }}
}}
"
    )
}

/// An error type is spelled as the Rust type itself: it is rendered through
/// `Display`, never carried across as a value.
fn error_ty(ty: &Ty, krate: &str) -> String {
    match ty {
        Ty::Str => "::std::string::String".into(),
        Ty::Opaque(path) => format!("::{path}"),
        Ty::Class(name) => format!("{krate}::{name}"),
        other => other.render(),
    }
}

fn class_block(class: &Class, krate: &str, plan: &BindingPlan) -> String {
    if class.is_plain_enum() {
        return plain_enum(class, krate);
    }
    let unsendable = if class.send { "" } else { ", unsendable" };
    let mut out = format!(
        "\n{}#[pyclass(name = \"{}\"{unsendable})]\npub struct {}({});\n",
        docs(class.doc.as_deref(), ""),
        class.name,
        class.name,
        inner_path(&class.rust_path, krate),
    );

    let mut members: Vec<String> = Vec::new();
    if let Some(ctor) = &class.ctor {
        members.push(constructor(ctor, krate, plan, class));
    }
    for accessor in &class.accessors {
        members.push(getter(accessor, plan));
        members.push(setter(accessor, plan));
    }
    for method in &class.methods {
        members.push(member(method, plan, class));
    }
    for associated in &class.statics {
        members.push(associated_fn(associated, krate, plan, class));
    }
    if !members.is_empty() {
        let _ = write!(
            out,
            "\n#[pymethods]\nimpl {} {{\n{}}}\n",
            class.name,
            members.join("\n")
        );
    }
    out
}

/// A plain enumeration mirrors onto a Python enum rather than staying an
/// opaque handle, with conversions both ways so it crosses in either
/// direction.
fn plain_enum(class: &Class, krate: &str) -> String {
    let inner = inner_path(&class.rust_path, krate);
    let name = &class.name;
    let names: Vec<&str> = class
        .variants
        .iter()
        .flatten()
        .map(|v| v.name.as_str())
        .collect();
    let arms = |from: &str, to: &str| -> String {
        names
            .iter()
            .map(|n| format!("            {from}::{n} => {to}::{n},\n"))
            .collect()
    };
    format!(
        "
{}#[pyclass(name = \"{name}\", eq, eq_int)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum {name} {{
{}}}

impl ::std::convert::From<{inner}> for {name} {{
    fn from(value: {inner}) -> Self {{
        match value {{
{}        }}
    }}
}}

impl ::std::convert::From<{name}> for {inner} {{
    fn from(value: {name}) -> Self {{
        match value {{
{}        }}
    }}
}}
",
        docs(class.doc.as_deref(), ""),
        names
            .iter()
            .map(|n| format!("    {n},\n"))
            .collect::<String>(),
        arms(&inner, name),
        arms(name, &inner),
    )
}

fn constructor(ctor: &Function, krate: &str, plan: &BindingPlan, owner: &Class) -> String {
    let detach = offloadable(ctor, Some(owner), plan);
    let call = format!(
        "{}::{}({})",
        inner_path(owner_path(&ctor.rust_path), krate),
        ctor.name,
        call_args(ctor, plan)
    );
    let ret = match fallible(ctor, plan) {
        true => "PyResult<Self>",
        false => "Self",
    };
    format!(
        "{}    #[new]\n    fn new({}) -> {ret} {{\n{}    }}\n",
        docs(ctor.doc.as_deref(), "    "),
        params(ctor, plan, detach),
        body(&call, ctor, plan, "        ", detach),
    )
}

/// A getter clones, which every bound field type supports: an exported type
/// held by a field never reaches here (the plan reports it instead).
fn getter(accessor: &Accessor, plan: &BindingPlan) -> String {
    let name = py_ident(&accessor.field);
    let field = &accessor.field;
    format!(
        "{}    #[getter]\n    fn {name}(&self) -> {} {{\n        {}\n    }}\n",
        docs(accessor.doc.as_deref(), "    "),
        returned_ty(&accessor.ty),
        returned(&format!("self.0.{field}.clone()"), &accessor.ty, plan),
    )
}

fn setter(accessor: &Accessor, plan: &BindingPlan) -> String {
    let name = py_ident(&accessor.field);
    let field = &accessor.field;
    let (spelling, assigned) = match &accessor.ty {
        Ty::List(inner) if buffered(inner).is_some() => {
            (view_name(inner, false), "value.into_vec()".to_string())
        }
        ty => (signature_ty(ty), into(ty, plan)),
    };
    format!(
        "    #[setter]\n    fn set_{name}(&mut self, value: {spelling}) {{\n        self.0.{field} = {assigned};\n    }}\n",
    )
}

fn member(method: &Function, plan: &BindingPlan, owner: &Class) -> String {
    let detach = offloadable(method, Some(owner), plan);
    let receiver = match method.receiver {
        Receiver::Exclusive => "&mut self",
        _ => "&self",
    };
    let call = format!("self.0.{}({})", method.name, call_args(method, plan));
    let args = join(receiver, &params(method, plan, detach));
    format!(
        "{}    {}fn {}({args}) -> {} {{\n{}    }}\n",
        docs(method.doc.as_deref(), "    "),
        asyncness(method),
        py_ident(&method.name),
        return_ty(method, plan),
        body(&call, method, plan, "        ", detach),
    )
}

fn associated_fn(function: &Function, krate: &str, plan: &BindingPlan, owner: &Class) -> String {
    let detach = offloadable(function, Some(owner), plan);
    let call = format!(
        "{}::{}({})",
        inner_path(owner_path(&function.rust_path), krate),
        function.name,
        call_args(function, plan)
    );
    format!(
        "{}    #[staticmethod]\n    {}fn {}({}) -> {} {{\n{}    }}\n",
        docs(function.doc.as_deref(), "    "),
        asyncness(function),
        py_ident(&function.name),
        params(function, plan, detach),
        return_ty(function, plan),
        body(&call, function, plan, "        ", detach),
    )
}

fn free_fn(function: &Function, krate: &str, plan: &BindingPlan) -> String {
    let detach = offloadable(function, None, plan);
    let call = format!(
        "{}({})",
        inner_path(&function.rust_path, krate),
        call_args(function, plan)
    );
    format!(
        "\n{}#[pyfunction]\n{}fn {}({}) -> {} {{\n{}}}\n",
        docs(function.doc.as_deref(), ""),
        asyncness(function),
        py_ident(&function.name),
        params(function, plan, detach),
        return_ty(function, plan),
        body(&call, function, plan, "    ", detach),
    )
}

/// The call, converted into whatever the generated signature promised: a
/// handle constructor, a mirrored value, a `?` through the error newtype.
/// [`aliasing_check`]'s `if`s run first, ahead of the call itself, whether
/// detached or not.
fn body(call: &str, function: &Function, plan: &BindingPlan, indent: &str, detach: bool) -> String {
    let call = match function.is_async {
        true => asyncrt::awaited(call),
        false => call.to_string(),
    };
    let checks = aliasing_check(function, plan, indent);
    if !detach {
        return format!("{checks}{}", completed(&call, function, plan, indent, ""));
    }
    // A call that cannot fail and needs no conversion is the whole body, so
    // binding its value would only name it to return it.
    if function.throws.is_none()
        && checks.is_empty()
        && returned("out", &function.ret, plan) == "out"
    {
        return format!("{indent}py.detach(|| {call})\n");
    }
    let bound = format!("{checks}{indent}let out = py.detach(|| {call});\n");
    completed("out", function, plan, indent, &bound)
}

/// The call converted into whatever the signature promised, after whatever
/// had to happen before it.
fn completed(
    expr: &str,
    function: &Function,
    plan: &BindingPlan,
    indent: &str,
    prefix: &str,
) -> String {
    let value = match &function.throws {
        Some(err) => format!("{expr}.map_err({})?", error_name(err)),
        None => expr.to_string(),
    };
    let out = returned(&value, &function.ret, plan);
    match fallible(function, plan) {
        true => format!("{prefix}{indent}Ok({out})\n"),
        false => format!("{prefix}{indent}{out}\n"),
    }
}

/// A value going the other way, back into the type the user's crate holds.
fn into(ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Class(name) if plan.is_mirrored(name) => "value.into()".into(),
        _ => "value".into(),
    }
}

/// A value leaving the user's crate, spelled the way the signature promised.
fn out(expr: &str, ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Class(name) if plan.is_mirrored(name) => format!("{expr}.into()"),
        Ty::Class(name) => format!("{name}({expr})"),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) if plan.is_mirrored(name) => {
                format!("{expr}.map(::std::convert::Into::into)")
            }
            Ty::Class(name) => format!("{expr}.map({name})"),
            _ => expr.to_string(),
        },
        _ => expr.to_string(),
    }
}

/// A returned type, where a sequence of one primitive comes back as its array
/// class rather than a list of boxed numbers.
fn returned_ty(ty: &Ty) -> String {
    match ty {
        Ty::Optional(inner) => format!("Option<{}>", returned_ty(inner)),
        _ => array_name(ty).unwrap_or_else(|| signature_ty(ty)),
    }
}

/// The same value, wrapped in whatever [`returned_ty`] promised.
fn returned(expr: &str, ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Optional(inner) => match array_name(inner) {
            Some(array) => format!("{expr}.map({array}::new)"),
            None => out(expr, ty, plan),
        },
        _ => match array_name(ty) {
            Some(array) => format!("{array}::new({expr})"),
            None => out(expr, ty, plan),
        },
    }
}

fn module_block(plan: &BindingPlan, opts: &BindOptions) -> String {
    let mut body = String::new();
    for (name, _) in buffers::arrays(plan) {
        let _ = writeln!(body, "    m.add_class::<{name}>()?;");
    }
    for class in &plan.classes {
        let _ = writeln!(body, "    m.add_class::<{}>()?;", class.name);
    }
    for function in &plan.functions {
        let _ = writeln!(
            body,
            "    m.add_function(wrap_pyfunction!({}, m)?)?;",
            py_ident(&function.name)
        );
    }
    format!(
        "\n#[pymodule]\nfn {}(m: &Bound<'_, PyModule>) -> PyResult<()> {{\n{body}    Ok(())\n}}\n",
        opts.module,
    )
}

/// The generated signature's arguments.
///
/// A `Python` token is not part of the Python signature: pyo3 recognises the
/// type and supplies it, so a released lock costs the caller no argument.
fn params(function: &Function, plan: &BindingPlan, detach: bool) -> String {
    let token = detach.then(|| "py: Python<'_>".to_string());
    token
        .into_iter()
        .chain(function.params.iter().map(|p| {
            let binding = match needs_mut(p, plan) {
                true => format!("mut {}", py_ident(&p.name)),
                false => py_ident(&p.name),
            };
            format!("{binding}: {}", passing(p, plan).0)
        }))
        .collect::<Vec<_>>()
        .join(", ")
}

fn call_args(function: &Function, plan: &BindingPlan) -> String {
    function
        .params
        .iter()
        .map(|p| passing(p, plan).1)
        .collect::<Vec<_>>()
        .join(", ")
}

/// How one parameter is spelled and how it is handed on, decided together so
/// the signature and the call cannot disagree.
///
/// Every sequence of a buffer-protocol type goes through a view, which is
/// what keeps a caller's buffer off the per-element unboxing path. Borrowing
/// one costs nothing at all; owning one costs a single memcpy.
fn passing(param: &Param, plan: &BindingPlan) -> (String, String) {
    let name = py_ident(&param.name);
    let class = match &param.ty {
        Ty::Class(c) => c.clone(),
        _ => String::new(),
    };
    match Transfer::of(param, plan) {
        Transfer::Handle { mirrored: true, .. } => (class, mirrored_arg(&name, param.ownership)),
        Transfer::Handle { writable: true, .. } => {
            (format!("PyRefMut<'_, {class}>"), format!("&mut {name}.0"))
        }
        Transfer::Handle { .. } => (format!("PyRef<'_, {class}>"), format!("&{name}.0")),
        Transfer::Text {
            nullable: true,
            borrowed,
        } => (
            signature_ty(&param.ty),
            match borrowed {
                true => format!("{name}.as_deref()"),
                false => name,
            },
        ),
        Transfer::Text {
            borrowed: true,
            nullable: false,
        } => ("&str".into(), name),
        Transfer::Buffer {
            element,
            borrowed,
            writable,
        } if buffered(&element.into()).is_some() => {
            let view = view_name(&element.into(), writable);
            let handoff = match (borrowed, writable) {
                (true, true) => format!("{name}.as_mut_slice()"),
                (true, false) => format!("{name}.as_slice()"),
                (false, _) => format!("{name}.into_vec()"),
            };
            (view, handoff)
        }
        _ => match param.ownership {
            Ownership::Owned => (signature_ty(&param.ty), name),
            Ownership::Borrowed => (signature_ty(&param.ty), format!("&{name}")),
            Ownership::BorrowedMut => (signature_ty(&param.ty), format!("&mut {name}")),
        },
    }
}

/// A mirrored enum always converts by value; a borrowed or exclusively
/// borrowed parameter needs a reference to that temporary instead of the
/// value itself.
fn mirrored_arg(name: &str, ownership: Ownership) -> String {
    let value = format!("{name}.into()");
    match ownership {
        Ownership::Owned => value,
        Ownership::Borrowed => format!("&{value}"),
        Ownership::BorrowedMut => format!("&mut {value}"),
    }
}

/// A writable buffer view is taken through `&mut self`, so its binding is
/// `mut`.
fn needs_mut(param: &Param, plan: &BindingPlan) -> bool {
    matches!(
        Transfer::of(param, plan),
        Transfer::Buffer { element, writable: true, .. } if buffered(&element.into()).is_some()
    )
}

fn return_ty(function: &Function, plan: &BindingPlan) -> String {
    let ok = returned_ty(&function.ret);
    match fallible(function, plan) {
        true => format!("PyResult<{ok}>"),
        false => ok,
    }
}

/// A type as it appears in generated signatures, where an exported type is
/// its wrapper rather than the Rust type the wrapper holds.
fn signature_ty(ty: &Ty) -> String {
    let nest = |inner: &Ty| signature_ty(inner);
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

/// A registry id as a path the glue crate can call, with the package's own
/// crate name replaced by the dependency's.
fn inner_path(id: &str, krate: &str) -> String {
    let tail = id.split_once("::").map(|(_, rest)| rest).unwrap_or(id);
    format!("{krate}::{tail}")
}

fn owner_path(rust_path: &str) -> &str {
    rust_path
        .rsplit_once("::")
        .map(|(head, _)| head)
        .unwrap_or(rust_path)
}

fn asyncness(function: &Function) -> &'static str {
    if function.is_async { "async " } else { "" }
}

fn join(receiver: &str, params: &str) -> String {
    match (receiver.is_empty(), params.is_empty()) {
        (true, _) => params.to_string(),
        (false, true) => receiver.to_string(),
        (false, false) => format!("{receiver}, {params}"),
    }
}

fn docs(doc: Option<&str>, indent: &str) -> String {
    match doc {
        Some(text) => format!("{indent}/// {text}\n"),
        None => String::new(),
    }
}
