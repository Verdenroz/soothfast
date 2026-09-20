//! The glue crate's `src/lib.rs`.
//!
//! Every exported type becomes a locally defined wrapper: `#[pyclass]`
//! expands to trait impls the orphan rule only permits in the crate that
//! defines the type. The same rule is why a failing call raises through a
//! local error newtype rather than `impl From<UserError> for PyErr`.

use std::fmt::Write;

use crate::model::{Ownership, Param, Receiver, Ty};
use crate::plan::{Accessor, BindingPlan, Class, Function, Transfer, offloadable};
use crate::{BindOptions, GENERATED_RS, GLUE_ALLOW};

use super::asyncrt;
use super::blocking;
use super::buffers::{self, array_name, buffered, view_name};
use super::errors::{self, Hierarchy, error_impl, error_name, error_newtypes};
use super::py_ident;
use super::{repr, seq, text};

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
pub(crate) fn render(plan: &BindingPlan, opts: &BindOptions) -> Result<String, String> {
    let krate = format!("::{}", opts.crate_name);
    let hierarchy = errors::hierarchy(plan, opts)?;
    blocking::check(plan, opts)?;
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
    for class in seq::classes(plan) {
        out.push_str(&seq::render(class, &krate));
    }
    out.push_str(&hierarchy.declarations(opts));
    for (ty, name) in error_newtypes(plan) {
        out.push_str(&error_impl(&ty, &name, &krate, &hierarchy));
    }
    for class in &plan.classes {
        out.push_str(&class_block(class, &krate, plan, opts));
    }
    for function in &plan.functions {
        out.push_str(&free_fn(function, &krate, plan));
        out.extend(blocking::free(function, &krate, plan, opts));
    }
    out.push_str(&module_block(plan, opts, &hierarchy));
    Ok(out)
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

/// Guards `x.method(x)`: a parameter of the receiver's own consumable class
/// shares its lock with `self`, and locking it twice in the one call would
/// deadlock a plain `std::sync::Mutex` rather than raise the usual
/// already-consumed error. Runs ahead of every other check, including
/// `self_access`'s own lock, since that is the lock this one exists to
/// keep from being taken twice.
fn self_alias_check(
    function: &Function,
    owner: Option<&Class>,
    plan: &BindingPlan,
    indent: &str,
) -> String {
    let Some(owner) = owner else {
        return String::new();
    };
    if !plan.is_consumable(&owner.name) {
        return String::new();
    }
    let mut out = String::new();
    for param in &function.params {
        let (class, optional) = match &param.ty {
            Ty::Class(name) => (Some(name), false),
            Ty::Optional(inner) => match &**inner {
                Ty::Class(name) => (Some(name), true),
                _ => (None, false),
            },
            _ => (None, false),
        };
        if class != Some(&owner.name) {
            continue;
        }
        let name = py_ident(&param.name);
        let condition = match optional {
            true => format!("{name}.as_deref().is_some_and(|v| ::std::ptr::eq(self, v))"),
            false => format!("::std::ptr::eq(self, &*{name})"),
        };
        let _ = writeln!(
            out,
            "{indent}if {condition} {{\n{indent}    return Err(::pyo3::exceptions::PyValueError::new_err(\"{name} aliases self\"));\n{indent}}}"
        );
    }
    out
}

/// Whether the generated signature has to return `PyResult`: the call
/// itself can fail, an aliasing check ahead of it can, or a mapped
/// parameter has to parse first.
/// A consuming method's owner is never passed here (see `consuming_member`);
/// any other method on a class a consuming method can empty always raises,
/// since the class may already be consumed by the time it runs, and the
/// same holds for a parameter of such a class wherever it appears.
fn fallible(function: &Function, plan: &BindingPlan, owner: Option<&Class>) -> bool {
    function.throws.is_some()
        || !aliasing_pairs(function, plan).is_empty()
        || function.params.iter().any(|p| text::mentions(&p.ty))
        || owner.is_some_and(|c| plan.is_consumable(&c.name))
        || function
            .params
            .iter()
            .any(|p| consumable_class_of(&p.ty, plan).is_some())
}

/// The consumable class a parameter's type names, if any: `Class` itself or
/// `Option<Class>`, the two shapes [`passing`] locks through the class's
/// own slot.
fn consumable_class_of<'a>(ty: &'a Ty, plan: &BindingPlan) -> Option<&'a str> {
    let name = match ty {
        Ty::Class(name) => name,
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) => name,
            _ => return None,
        },
        _ => return None,
    };
    plan.is_consumable(name).then_some(name.as_str())
}

fn class_block(class: &Class, krate: &str, plan: &BindingPlan, opts: &BindOptions) -> String {
    if class.is_plain_enum() {
        return plain_enum(class, krate);
    }
    let unsendable = if class.send { "" } else { ", unsendable" };
    let consumable = plan.is_consumable(&class.name);
    // A by-value `#[pyclass]` parameter needs the wrapper `Clone` and
    // pyo3's opt-in `FromPyObject` for it; the blanket impl is being
    // retired. A consumable wrapper holds its value behind a `Mutex`,
    // which is never `Clone`, so the two features do not combine.
    let (derive_clone, from_py_object) = if class.clone && !consumable {
        ("#[derive(Clone)]\n", ", from_py_object")
    } else {
        ("", "")
    };
    let inner = inner_path(&class.rust_path, krate);
    let field = match consumable {
        true => format!("::std::sync::Mutex<{}>", slot_name(&class.name)),
        false => inner.clone(),
    };
    let mut out = format!(
        "\n{}#[pyclass(name = \"{}\"{unsendable}{from_py_object})]\n{derive_clone}pub struct {}({field});\n",
        docs(class.doc.as_deref(), ""),
        class.name,
        class.name,
    );
    if consumable {
        out.push_str(&slot_enum(class, &inner));
        out.push_str(&take_impl(class, &inner));
    }

    let mut members: Vec<String> = Vec::new();
    if let Some(ctor) = &class.ctor {
        members.push(constructor(ctor, krate, plan, class));
    }
    for accessor in &class.accessors {
        members.push(getter(accessor, plan, class));
        members.extend(setter(accessor, plan, class));
    }
    members.extend(repr::member(class, plan));
    for method in &class.methods {
        members.push(member(method, plan, class));
        members.extend(blocking::member(method, plan, class, opts));
    }
    for associated in &class.statics {
        members.push(associated_fn(associated, krate, plan, class));
        members.extend(blocking::associated(associated, krate, plan, class, opts));
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

/// The enum a consumable class's lock guards: its own value, or the name of
/// whichever consuming method last took it out.
pub(super) fn slot_name(class_name: &str) -> String {
    format!("{class_name}Slot")
}

fn slot_enum(class: &Class, inner: &str) -> String {
    format!(
        "\nenum {} {{\n    Live({inner}),\n    Consumed(&'static str),\n}}\n",
        slot_name(&class.name),
    )
}

/// `take` empties the slot for a consuming method to move the value out of.
/// A call that finds it already empty restores the name already there
/// rather than overwriting it, so the error always names the first method
/// that consumed the handle, not whichever tried to reuse it since.
fn take_impl(class: &Class, inner: &str) -> String {
    let name = &class.name;
    let slot = slot_name(name);
    format!(
        "\nimpl {name} {{\n    fn take(&self, consumer: &'static str) -> ::pyo3::PyResult<{inner}> {{\n        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());\n        match ::std::mem::replace(&mut *guard, {slot}::Consumed(consumer)) {{\n            {slot}::Live(value) => Ok(value),\n            {slot}::Consumed(by) => {{\n                *guard = {slot}::Consumed(by);\n                Err(::pyo3::exceptions::PyRuntimeError::new_err(format!(\"{name} was consumed by {{by}}\")))\n            }}\n        }}\n    }}\n}}\n",
    )
}

/// Where a getter, setter, method or repr reads a class's inner value:
/// `self.0` directly for a plain wrapper, or a prelude that locks a
/// consumable one's slot and matches out the live value, bound as `inner`.
/// The lock never crosses an `.await`: `take` (see `take_impl`) drops its
/// own guard before returning, and every other accessor here is
/// synchronous start to finish.
pub(super) struct SelfAccess {
    pub(super) prelude: String,
    pub(super) expr: &'static str,
}

pub(super) fn self_access(
    owner: &Class,
    plan: &BindingPlan,
    indent: &str,
    mutable: bool,
) -> SelfAccess {
    if !plan.is_consumable(&owner.name) {
        return SelfAccess {
            prelude: String::new(),
            expr: "self.0",
        };
    }
    let slot = slot_name(&owner.name);
    let name = &owner.name;
    let (binding, deref) = match mutable {
        true => ("let mut guard", "&mut *guard"),
        false => ("let guard", "&*guard"),
    };
    let prelude = format!(
        "{indent}{binding} = self.0.lock().unwrap_or_else(|e| e.into_inner());\n{indent}let inner = match {deref} {{\n{indent}    {slot}::Live(value) => value,\n{indent}    {slot}::Consumed(by) => return Err(::pyo3::exceptions::PyRuntimeError::new_err(format!(\"{name} was consumed by {{by}}\"))),\n{indent}}};\n",
    );
    SelfAccess {
        prelude,
        expr: "inner",
    }
}

/// A parameter of a class a consuming method can empty, read through its
/// own lock inline rather than through a `let` prelude: `binding` is the
/// `Mutex` field access (`name.0`, or `v.0` inside an `Option::map`
/// closure), and the temporary guard it locks lives for the whole
/// statement the call sits in, same as any other match scrutinee.
fn locked_param(binding: &str, class: &str, mutable: bool) -> String {
    let slot = slot_name(class);
    let deref = if mutable { "&mut *" } else { "&*" };
    format!(
        "match {deref}{binding}.lock().unwrap_or_else(|e| e.into_inner()) {{ {slot}::Live(value) => value, {slot}::Consumed(by) => return Err(::pyo3::exceptions::PyRuntimeError::new_err(format!(\"{class} was consumed by {{by}}\"))) }}"
    )
}

/// The prelude an `Option<&Class>` parameter of a consumable class needs:
/// unlike `self_access`'s, its guard cannot live in the call expression
/// itself (a `MutexGuard` a closure creates and returns through does not
/// outlive that closure), so it is bound by a `let` first, shadowing the
/// original `PyRef`/`PyRefMut` binding with the plain reference the call
/// site wants.
fn param_prelude(param: &Param, plan: &BindingPlan, indent: &str) -> String {
    let Ty::Optional(inner) = &param.ty else {
        return String::new();
    };
    let Ty::Class(class) = &**inner else {
        return String::new();
    };
    if !plan.is_consumable(class) {
        return String::new();
    }
    let name = py_ident(&param.name);
    let slot = slot_name(class);
    let mutable = param.inner_ownership == Ownership::BorrowedMut;
    let (bind, deref2) = match mutable {
        true => (format!("let mut {name}"), "as_deref_mut"),
        false => (format!("let {name}"), "as_deref"),
    };
    format!(
        "{indent}{bind} = {name}.as_deref().map(|v| v.0.lock().unwrap_or_else(|e| e.into_inner()));\n{indent}let {name} = {name}.{deref2}().map(|value| match value {{\n{indent}    {slot}::Live(v) => Ok(v),\n{indent}    {slot}::Consumed(by) => Err(::pyo3::exceptions::PyRuntimeError::new_err(format!(\"{class} was consumed by {{by}}\"))),\n{indent}}}).transpose()?;\n"
    )
}

fn param_preludes(function: &Function, plan: &BindingPlan, indent: &str) -> String {
    function
        .params
        .iter()
        .map(|p| param_prelude(p, plan, indent))
        .collect()
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
    // `inner` may be `#[non_exhaustive]`, so every conversion out of it
    // carries a catch-all arm, unreachable for a matching binding.
    let arms = |from: &str, to: &str, catch_all: bool| -> String {
        let mut out: String = names
            .iter()
            .map(|n| format!("            {from}::{n} => {to}::{n},\n"))
            .collect();
        if catch_all {
            let _ = writeln!(
                out,
                "            _ => ::std::unreachable!(\"{from} gained a variant this binding was not generated for\"),"
            );
        }
        out
    };
    format!(
        "
{}#[pyclass(name = \"{name}\", eq, eq_int, from_py_object)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum {name} {{
{}}}

impl ::std::convert::From<{inner}> for {name} {{
    #[allow(unreachable_patterns)]
    fn from(value: {inner}) -> Self {{
        match value {{
{}        }}
    }}
}}

// A mirrored enum has no derived `Clone`; a field getter converts through
// this one instead of cloning an owned copy just to consume it.
impl ::std::convert::From<&{inner}> for {name} {{
    #[allow(unreachable_patterns)]
    fn from(value: &{inner}) -> Self {{
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
        arms(&inner, name, true),
        arms(&inner, name, true),
        arms(name, &inner, false),
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
    let ret = match fallible(ctor, plan, None) {
        true => "PyResult<Self>",
        false => "Self",
    };
    format!(
        "{}    #[new]\n    fn new({}) -> {ret} {{\n{}    }}\n",
        docs(ctor.doc.as_deref(), "    "),
        params(ctor, plan, detach),
        body(&call, ctor, plan, "        ", detach, "", None),
    )
}

/// A getter clones, which every bound field type supports: an exported type
/// reaches here only when the plan found it `Clone`, and is wrapped into a
/// fresh handle. A sequence of one is wrapped into its seq instead.
fn getter(accessor: &Accessor, plan: &BindingPlan, owner: &Class) -> String {
    if let Some(class) = seq::element(&accessor.ty, plan) {
        return seq::getter(accessor, class);
    }
    let consumable = plan.is_consumable(&owner.name);
    let access = self_access(owner, plan, "        ", false);
    let expr = access.expr;
    let name = py_ident(&accessor.field);
    let field = &accessor.field;
    let read = match &accessor.ty {
        Ty::Class(class) if plan.is_mirrored(class) => format!("(&{expr}.{field})"),
        _ => format!("{expr}.{field}.clone()"),
    };
    let body = text::out_ref(&format!("{expr}.{field}"), &accessor.ty)
        .unwrap_or_else(|| returned(&read, &accessor.ty, plan));
    let ret = returned_ty(&accessor.ty);
    let (ret, body) = match consumable {
        true => (format!("PyResult<{ret}>"), format!("Ok({body})")),
        false => (ret, body),
    };
    format!(
        "{}    #[getter]\n    fn {name}(&self) -> {ret} {{\n{}        {body}\n    }}\n",
        docs(accessor.doc.as_deref(), "    "),
        access.prelude,
    )
}

/// A field holding an exported type has no setter: the handle Python passes
/// in cannot be moved out of, and the type is not known to be `Clone`.
fn setter(accessor: &Accessor, plan: &BindingPlan, owner: &Class) -> Option<String> {
    if holds_handle(&accessor.ty, plan) {
        return None;
    }
    let consumable = plan.is_consumable(&owner.name);
    let access = self_access(owner, plan, "        ", true);
    let receiver = if consumable { "&self" } else { "&mut self" };
    let expr = access.expr;
    let name = py_ident(&accessor.field);
    let field = &accessor.field;
    let (spelling, assigned) = match &accessor.ty {
        Ty::List(inner) if buffered(inner).is_some() => {
            (view_name(inner, false), "value.into_vec()".to_string())
        }
        ty => (signature_ty(ty), into(ty, plan)),
    };
    let mapped = text::parse("value", &accessor.ty);
    let ok_needed = consumable || mapped.is_some();
    let assign = match mapped {
        Some(parsed) => format!("{expr}.{field} = {parsed};"),
        None => format!("{expr}.{field} = {assigned};"),
    };
    let (ret, tail) = match ok_needed {
        true => (" -> PyResult<()>", format!("{assign}\n        Ok(())")),
        false => ("", assign),
    };
    Some(format!(
        "    #[setter]\n    fn set_{name}({receiver}, value: {spelling}){ret} {{\n{}        {tail}\n    }}\n",
        access.prelude,
    ))
}

pub(super) fn holds_handle(ty: &Ty, plan: &BindingPlan) -> bool {
    match ty {
        Ty::Class(name) => !plan.is_mirrored(name),
        Ty::Optional(inner) | Ty::List(inner) => holds_handle(inner, plan),
        Ty::Map(key, value) => holds_handle(key, plan) || holds_handle(value, plan),
        Ty::Tuple(items) => items.iter().any(|t| holds_handle(t, plan)),
        _ => false,
    }
}

fn member(method: &Function, plan: &BindingPlan, owner: &Class) -> String {
    if method.receiver == Receiver::Consuming {
        return consuming_member(method, plan, owner);
    }
    let detach = offloadable(method, Some(owner), plan);
    let consumable = plan.is_consumable(&owner.name);
    let mutable = method.receiver == Receiver::Exclusive;
    let access = self_access(owner, plan, "        ", mutable);
    let receiver = match consumable {
        true => "&self",
        false if mutable => "&mut self",
        false => "&self",
    };
    let call = format!(
        "{}.{}({})",
        access.expr,
        method.name,
        call_args(method, plan)
    );
    let args = join(receiver, &params(method, plan, detach));
    format!(
        "{}    {}fn {}({args}) -> {} {{\n{}    }}\n",
        docs(method.doc.as_deref(), "    "),
        asyncness(method),
        py_ident(&method.name),
        return_ty(method, plan, Some(owner)),
        body(
            &call,
            method,
            plan,
            "        ",
            detach,
            &access.prelude,
            Some(owner)
        ),
    )
}

/// A method that takes `self` by value: `take`s the value out of the
/// wrapper's slot, calls it, and either refills the slot (a `Self`-returning
/// setter, so `b.region(r).lang(l)` chains and mutates in place) or leaves
/// it consumed (a terminal call like `build`). `take`'s own lock is dropped
/// before it returns, so a terminal call is free to `.await` afterward.
fn consuming_member(method: &Function, plan: &BindingPlan, owner: &Class) -> String {
    let is_self_return = matches!(&method.ret, Ty::Class(name) if name == &owner.name);
    let inner_call = format!("value.{}({})", method.name, call_args(method, plan));
    let docs = docs(method.doc.as_deref(), "    ");
    let asyncness = asyncness(method);
    let name = py_ident(&method.name);
    if is_self_return {
        let slot = slot_name(&owner.name);
        let call = match &method.throws {
            Some(err) => format!("{inner_call}.map_err({})?", error_name(err)),
            None => inner_call,
        };
        let args = join("slf: PyRef<'_, Self>", &params(method, plan, false));
        return format!(
            "{docs}    fn {name}({args}) -> PyResult<PyRef<'_, Self>> {{\n        let value = slf.take(\"{}\")?;\n        let value = {call};\n        *slf.0.lock().unwrap_or_else(|e| e.into_inner()) = {slot}::Live(value);\n        Ok(slf)\n    }}\n",
            method.name,
        );
    }
    // `take` returns before the caller ever awaits anything, so `&self`
    // holds no lock across the `.await` below (pyo3's async pymethods only
    // accept `&self`/`&mut self` receivers in any case).
    let take = format!("self.take(\"{}\")?", method.name);
    let args = join("&self", &params(method, plan, false));
    let ret = return_ty(method, plan, Some(owner));
    let call = match method.is_async {
        true => asyncrt::awaited(&inner_call),
        false => inner_call,
    };
    let body = format!(
        "        let value = {take};\n{}",
        completed(&call, method, plan, "        ", "", Some(owner))
    );
    format!("{docs}    {asyncness}fn {name}({args}) -> {ret} {{\n{body}    }}\n")
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
        return_ty(function, plan, None),
        body(&call, function, plan, "        ", detach, "", None),
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
        return_ty(function, plan, None),
        body(&call, function, plan, "    ", detach, "", None),
    )
}

/// The call, converted into whatever the generated signature promised: a
/// handle constructor, a mirrored value, a `?` through the error newtype.
/// `prelude` runs first (a consumable owner's lock, see `self_access`), then
/// [`aliasing_check`]'s `if`s, whether detached or not.
#[allow(clippy::too_many_arguments)]
fn body(
    call: &str,
    function: &Function,
    plan: &BindingPlan,
    indent: &str,
    detach: bool,
    prelude: &str,
    owner: Option<&Class>,
) -> String {
    let call = match function.is_async {
        true => asyncrt::awaited(call),
        false => call.to_string(),
    };
    shaped_body(&call, function, plan, indent, detach, prelude, owner)
}

/// [`body`] over a call already shaped for how it runs: awaited, blocked on,
/// or plain.
#[allow(clippy::too_many_arguments)]
pub(super) fn shaped_body(
    call: &str,
    function: &Function,
    plan: &BindingPlan,
    indent: &str,
    detach: bool,
    prelude: &str,
    owner: Option<&Class>,
) -> String {
    let checks = format!(
        "{}{}{prelude}{}{}",
        self_alias_check(function, owner, plan, indent),
        param_preludes(function, plan, indent),
        aliasing_check(function, plan, indent),
        text::preludes(function, indent)
    );
    if !detach {
        return format!(
            "{checks}{}",
            completed(call, function, plan, indent, "", owner)
        );
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
    completed("out", function, plan, indent, &bound, owner)
}

/// The call converted into whatever the signature promised, after whatever
/// had to happen before it.
pub(super) fn completed(
    expr: &str,
    function: &Function,
    plan: &BindingPlan,
    indent: &str,
    prefix: &str,
    owner: Option<&Class>,
) -> String {
    let value = match &function.throws {
        Some(err) => format!("{expr}.map_err({})?", error_name(err)),
        None => expr.to_string(),
    };
    let value = match function.ret_borrowed {
        true => owned(&value, &function.ret),
        false => value,
    };
    let out = returned(&value, &function.ret, plan);
    match fallible(function, plan, owner) {
        true => format!("{prefix}{indent}Ok({out})\n"),
        false => format!("{prefix}{indent}{out}\n"),
    }
}

/// A value going the other way, back into the type the user's crate holds.
fn into(ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Class(name) if plan.is_mirrored(name) => "value.into()".into(),
        Ty::Optional(inner) if matches!(&**inner, Ty::Class(name) if plan.is_mirrored(name)) => {
            "value.map(::std::convert::Into::into)".into()
        }
        Ty::List(inner) if matches!(&**inner, Ty::Class(name) if plan.is_mirrored(name)) => {
            "value.into_iter().map(::std::convert::Into::into).collect()".into()
        }
        _ => "value".into(),
    }
}

/// A borrowed return, owned: `&str` and `&[T]` through `ToOwned`, recursing
/// through `Option`, a sequence, or a map's values wherever one nests a
/// borrow (`Vec<&str>`, `Option<Vec<&str>>`, a map with `&str` values).
fn owned(expr: &str, ty: &Ty) -> String {
    match ty {
        Ty::Optional(inner) if is_container(inner) => {
            format!("{expr}.map(|value| {})", owned("value", inner))
        }
        Ty::Optional(_) => format!("{expr}.map(::std::borrow::ToOwned::to_owned)"),
        Ty::List(inner) => format!(
            "{expr}.into_iter().map(|value| {}).collect()",
            owned("value", inner)
        ),
        Ty::Map(_, value) => format!(
            "{expr}.into_iter().map(|(key, value)| (key, {})).collect()",
            owned("value", value)
        ),
        _ => format!("{expr}.to_owned()"),
    }
}

fn is_container(ty: &Ty) -> bool {
    matches!(ty, Ty::List(_) | Ty::Optional(_) | Ty::Map(_, _))
}

/// A fresh handle, constructed from an owned value: a plain tuple call for
/// most classes, or a live slot for one a consuming method can later empty.
fn wrap_handle(name: &str, expr: &str, plan: &BindingPlan) -> String {
    match plan.is_consumable(name) {
        true => format!(
            "{name}(::std::sync::Mutex::new({}::Live({expr})))",
            slot_name(name)
        ),
        false => format!("{name}({expr})"),
    }
}

/// A value leaving the user's crate, spelled the way the signature promised.
fn out(expr: &str, ty: &Ty, plan: &BindingPlan) -> String {
    if let Some(rendered) = text::out(expr, ty) {
        return rendered;
    }
    match ty {
        Ty::Class(name) if plan.is_mirrored(name) => format!("{expr}.into()"),
        Ty::Class(name) => wrap_handle(name, expr, plan),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) if plan.is_mirrored(name) => {
                format!("{expr}.map(::std::convert::Into::into)")
            }
            Ty::Class(name) if plan.is_consumable(name) => {
                format!("{expr}.map(|value| {})", wrap_handle(name, "value", plan))
            }
            Ty::Class(name) => format!("{expr}.map({name})"),
            _ if has_class(inner) => format!("{expr}.map(|value| {})", out("value", inner, plan)),
            _ => expr.to_string(),
        },
        Ty::List(inner) => match &**inner {
            Ty::Class(name) if plan.is_mirrored(name) => {
                format!("{expr}.into_iter().map(::std::convert::Into::into).collect()")
            }
            Ty::Class(name) if plan.is_consumable(name) => format!(
                "{expr}.into_iter().map(|value| {}).collect()",
                wrap_handle(name, "value", plan)
            ),
            Ty::Class(name) => format!("{expr}.into_iter().map({name}).collect()"),
            _ if has_class(inner) => format!(
                "{expr}.into_iter().map(|value| {}).collect()",
                out("value", inner, plan)
            ),
            _ => expr.to_string(),
        },
        Ty::Map(_, value) if has_class(value) => format!(
            "{expr}.into_iter().map(|(key, value)| (key, {})).collect()",
            out("value", value, plan)
        ),
        Ty::Tuple(items) if items.iter().any(has_class) => tuple_out(expr, items, plan),
        _ => expr.to_string(),
    }
}

/// Whether a type nests an exported class at any depth, the signal `out`
/// uses to tell a composition worth converting from one that crosses as-is.
fn has_class(ty: &Ty) -> bool {
    match ty {
        Ty::Class(_) => true,
        Ty::Optional(inner) | Ty::List(inner) => has_class(inner),
        Ty::Map(_, value) => has_class(value),
        Ty::Tuple(items) => items.iter().any(has_class),
        _ => false,
    }
}

/// A tuple leaving the user's crate, destructured so each position converts
/// on its own terms.
fn tuple_out(expr: &str, items: &[Ty], plan: &BindingPlan) -> String {
    let names: Vec<String> = (0..items.len()).map(|i| format!("v{i}")).collect();
    let converted: Vec<String> = names
        .iter()
        .zip(items)
        .map(|(name, ty)| out(name, ty, plan))
        .collect();
    format!(
        "{{ let ({}) = {expr}; ({}) }}",
        names.join(", "),
        converted.join(", ")
    )
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

/// `gil_used = false` tells a free-threaded interpreter the module needs no
/// GIL, so importing it does not switch the GIL back on for the whole
/// process. It holds because nothing here shares mutable state outside
/// pyo3's own borrow checking: a handle's `&mut self` calls and setters go
/// through `PyRefMut`, a returned array is read-only through the buffer
/// protocol and never mutated after construction, and the runtime and its
/// relay are `Sync`. On a GIL build the flag is inert.
fn module_block(plan: &BindingPlan, opts: &BindOptions, hierarchy: &Hierarchy) -> String {
    let mut body = String::new();
    for (name, _) in buffers::arrays(plan) {
        let _ = writeln!(body, "    m.add_class::<{name}>()?;");
    }
    for class in seq::classes(plan) {
        let _ = writeln!(body, "    m.add_class::<{}>()?;", seq::name(&class.name));
    }
    for class in &plan.classes {
        let _ = writeln!(body, "    m.add_class::<{}>()?;", class.name);
    }
    body.push_str(&hierarchy.registrations());
    for function in &plan.functions {
        let _ = writeln!(
            body,
            "    m.add_function(wrap_pyfunction!({}, m)?)?;",
            py_ident(&function.name)
        );
    }
    body.push_str(&blocking::registrations(plan, opts));
    format!(
        "\n#[pymodule(gil_used = false)]\nfn {}(m: &Bound<'_, PyModule>) -> PyResult<()> {{\n{body}    Ok(())\n}}\n",
        opts.module,
    )
}

/// The generated signature's arguments.
///
/// A `Python` token is not part of the Python signature: pyo3 recognises the
/// type and supplies it, so a released lock costs the caller no argument.
pub(super) fn params(function: &Function, plan: &BindingPlan, detach: bool) -> String {
    let token = detach.then(|| "py: Python<'_>".to_string());
    token
        .into_iter()
        .chain(function.params.iter().map(|p| {
            let binding = match needs_mut(p, plan) {
                true => format!("mut {}", py_ident(&p.name)),
                false => py_ident(&p.name),
            };
            format!("{binding}: {}", passing(p, plan, function.is_async).0)
        }))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn call_args(function: &Function, plan: &BindingPlan) -> String {
    function
        .params
        .iter()
        .map(|p| passing(p, plan, function.is_async).1)
        .collect::<Vec<_>>()
        .join(", ")
}

/// How one parameter is spelled and how it is handed on, decided together so
/// the signature and the call cannot disagree.
///
/// Every sequence of a buffer-protocol type goes through a view, which is
/// what keeps a caller's buffer off the per-element unboxing path. Borrowing
/// one costs nothing at all; owning one costs a single memcpy.
fn passing(param: &Param, plan: &BindingPlan, is_async: bool) -> (String, String) {
    let name = py_ident(&param.name);
    let class = match &param.ty {
        Ty::Class(c) => c.clone(),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(c) => c.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    };
    match Transfer::of(param, plan) {
        Transfer::Handle {
            mirrored: true,
            nullable: true,
            ..
        } => (
            format!("Option<{class}>"),
            format!("{name}.map(::std::convert::Into::into)"),
        ),
        Transfer::Handle { mirrored: true, .. } => (class, mirrored_arg(&name, param.ownership)),
        Transfer::Handle {
            writable: true,
            nullable: true,
            ..
        } => {
            // A consumable class's prelude (see `param_prelude`) already
            // shadows `name` down to `Option<&mut Inner>`; the plain
            // `PyRefMut` binding still needs `&mut v.0` here.
            let expr = match plan.is_consumable(&class) {
                true => name.clone(),
                false => format!("{name}.as_deref_mut().map(|v| &mut v.0)"),
            };
            (format!("Option<PyRefMut<'_, {class}>>"), expr)
        }
        // A `PyRef` cannot cross an async fn's own await (it is tied to the
        // GIL, never `Send`); `bindable` only lets a `Clone` class reach
        // here, so pyo3 extracts an owned value in its place instead.
        Transfer::Handle { nullable: true, .. } if is_async => (
            format!("Option<{class}>"),
            format!("{name}.as_ref().map(|v| &v.0)"),
        ),
        Transfer::Handle { nullable: true, .. } => {
            let expr = match plan.is_consumable(&class) {
                true => name.clone(),
                false => format!("{name}.as_deref().map(|v| &v.0)"),
            };
            (format!("Option<PyRef<'_, {class}>>"), expr)
        }
        Transfer::Handle { writable: true, .. } => {
            let expr = match plan.is_consumable(&class) {
                true => locked_param(&format!("{name}.0"), &class, true),
                false => format!("&mut {name}.0"),
            };
            (format!("PyRefMut<'_, {class}>"), expr)
        }
        Transfer::Handle { .. } if is_async => (class, format!("&{name}.0")),
        Transfer::Handle { .. } => {
            let expr = match plan.is_consumable(&class) {
                true => locked_param(&format!("{name}.0"), &class, false),
                false => format!("&{name}.0"),
            };
            (format!("PyRef<'_, {class}>"), expr)
        }
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
        _ if list_mirrored_class(&param.ty, plan).is_some() => {
            let class_name = list_mirrored_class(&param.ty, plan).unwrap();
            (
                format!("Vec<{class_name}>"),
                format!("{name}.into_iter().map(::std::convert::Into::into).collect()"),
            )
        }
        // A mapped parameter was parsed and rebound ahead of the call, so
        // it is handed on like any owned value; `Option<&T>` re-borrows it.
        _ if text::mentions(&param.ty) && param.inner_ownership != Ownership::Owned => {
            (signature_ty(&param.ty), format!("{name}.as_ref()"))
        }
        _ => match param.ownership {
            Ownership::Owned => (signature_ty(&param.ty), name),
            Ownership::Borrowed => (signature_ty(&param.ty), format!("&{name}")),
            Ownership::BorrowedMut => (signature_ty(&param.ty), format!("&mut {name}")),
        },
    }
}

/// The mirrored class a `Vec<Class>` parameter carries element-wise, if any:
/// a non-mirrored one is already gapped in plan/mod.rs before this runs.
fn list_mirrored_class<'a>(ty: &'a Ty, plan: &BindingPlan) -> Option<&'a str> {
    match ty {
        Ty::List(inner) => match &**inner {
            Ty::Class(name) if plan.is_mirrored(name) => Some(name),
            _ => None,
        },
        _ => None,
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
    if consumable_class_of(&param.ty, plan).is_some() {
        return false;
    }
    matches!(
        Transfer::of(param, plan),
        Transfer::Buffer { element, writable: true, .. } if buffered(&element.into()).is_some()
    ) || matches!(
        Transfer::of(param, plan),
        Transfer::Handle { writable: true, .. }
    )
}

pub(super) fn return_ty(function: &Function, plan: &BindingPlan, owner: Option<&Class>) -> String {
    let ok = returned_ty(&function.ret);
    match fallible(function, plan, owner) {
        true => format!("PyResult<{ok}>"),
        false => ok,
    }
}

/// A type as it appears in generated signatures, where an exported type is
/// its wrapper rather than the Rust type the wrapper holds.
fn signature_ty(ty: &Ty) -> String {
    let nest = |inner: &Ty| signature_ty(inner);
    match ty {
        Ty::Str | Ty::Text(_) => "String".into(),
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
pub(super) fn inner_path(id: &str, krate: &str) -> String {
    let tail = id.split_once("::").map(|(_, rest)| rest).unwrap_or(id);
    format!("{krate}::{tail}")
}

pub(super) fn owner_path(rust_path: &str) -> &str {
    rust_path
        .rsplit_once("::")
        .map(|(head, _)| head)
        .unwrap_or(rust_path)
}

fn asyncness(function: &Function) -> &'static str {
    if function.is_async { "async " } else { "" }
}

pub(super) fn join(receiver: &str, params: &str) -> String {
    match (receiver.is_empty(), params.is_empty()) {
        (true, _) => params.to_string(),
        (false, true) => receiver.to_string(),
        (false, false) => format!("{receiver}, {params}"),
    }
}

pub(super) fn docs(doc: Option<&str>, indent: &str) -> String {
    match doc {
        Some(text) => format!("{indent}/// {text}\n"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Variant, VariantFields};

    /// `Level` on the Rust side may be `#[non_exhaustive]`; the plan carries
    /// no such flag, so the conversion out of it always needs a catch-all.
    fn flag_class() -> Class {
        Class {
            rust_path: "acme::Level".into(),
            name: "Level".into(),
            doc: None,
            send: true,
            sync: true,
            clone: false,
            ctor: None,
            accessors: Vec::new(),
            methods: Vec::new(),
            statics: Vec::new(),
            variants: Some(vec![
                Variant {
                    name: "Low".into(),
                    fields: VariantFields::Unit,
                    doc: None,
                },
                Variant {
                    name: "High".into(),
                    fields: VariantFields::Unit,
                    doc: None,
                },
            ]),
        }
    }

    #[test]
    fn a_plain_enum_conversion_out_of_the_foreign_type_has_a_catch_all_arm() {
        let rendered = plain_enum(&flag_class(), "::acme");
        let from_foreign = rendered
            .split("impl ::std::convert::From<::acme::Level> for Level")
            .nth(1)
            .expect("renders the owned conversion");
        assert!(
            from_foreign.contains("#[allow(unreachable_patterns)]"),
            "a catch-all arm is unreachable for a locally exhaustive enum: {rendered}"
        );
        assert!(
            from_foreign.contains("_ => ::std::unreachable!"),
            "without a catch-all this match fails to compile against a \
             `#[non_exhaustive]` source enum: {rendered}"
        );
        let from_reverse = rendered
            .split("impl ::std::convert::From<Level> for ::acme::Level")
            .nth(1)
            .expect("renders the reverse conversion");
        assert!(
            !from_reverse.contains("::std::unreachable!"),
            "the mirrored enum is exhaustive on its own, so the reverse \
             conversion needs no catch-all: {rendered}"
        );
    }
}
