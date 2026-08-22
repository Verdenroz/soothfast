//! The glue crate's `src/lib.rs`.
//!
//! Every exported type becomes a locally defined wrapper: `#[wasm_bindgen]`
//! expands to trait impls the orphan rule only permits in the crate that
//! defines the type. The same rule is why a failing call rejects through a
//! local error newtype rather than `impl From<UserError> for JsValue`.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::model::{Ownership, Param, Receiver, Ty};
use crate::naming::ident_part;
use crate::plan::{Accessor, BindingPlan, Class, Function, Transfer};
use crate::{BindOptions, GENERATED_RS, GLUE_ALLOW};

use super::js_ident;

/// Render the glue crate body.
pub(crate) fn render(plan: &BindingPlan, opts: &BindOptions) -> String {
    let krate = format!("::{}", opts.crate_name);
    let mut out = String::from(GENERATED_RS);
    out.push_str(GLUE_ALLOW);
    out.push_str("\nuse wasm_bindgen::prelude::*;\n");

    for (ty, name) in error_newtypes(plan) {
        out.push_str(&error_impl(&ty, &name, &krate));
    }
    for class in &plan.classes {
        out.push_str(&class_block(class, &krate, plan));
    }
    for function in &plan.functions {
        out.push_str(&free_fn(function, &krate, plan));
    }
    out
}

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

impl ::std::convert::From<{name}> for ::wasm_bindgen::JsValue {{
    fn from(err: {name}) -> ::wasm_bindgen::JsValue {{
        ::wasm_bindgen::JsValue::from_str(&::std::string::ToString::to_string(&err.0))
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
    let mut out = format!(
        "\n{}#[wasm_bindgen]\npub struct {}({});\n",
        docs(class.doc.as_deref(), ""),
        class.name,
        inner_path(&class.rust_path, krate),
    );

    let mut members: Vec<String> = Vec::new();
    if let Some(ctor) = &class.ctor {
        members.push(constructor(ctor, krate, plan));
    }
    for accessor in &class.accessors {
        members.push(getter(accessor, plan));
        members.push(setter(accessor, plan));
    }
    for method in &class.methods {
        members.push(member(method, plan));
    }
    for associated in &class.statics {
        members.push(associated_fn(associated, krate, plan));
    }
    if !members.is_empty() {
        let _ = write!(
            out,
            "\n#[wasm_bindgen]\nimpl {} {{\n{}}}\n",
            class.name,
            members.join("\n")
        );
    }
    out
}

/// A plain enumeration mirrors onto a JavaScript enum rather than staying an
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
{}#[wasm_bindgen]
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

fn constructor(ctor: &Function, krate: &str, plan: &BindingPlan) -> String {
    let call = format!(
        "{}::{}({})",
        inner_path(owner_path(&ctor.rust_path), krate),
        ctor.name,
        call_args(ctor, plan)
    );
    let ret = match ctor.throws {
        Some(_) => "Result<Self, JsValue>",
        None => "Self",
    };
    format!(
        "{}    #[wasm_bindgen(constructor)]\n    pub fn new({}) -> {ret} {{\n{}    }}\n",
        docs(ctor.doc.as_deref(), "    "),
        params(ctor, plan),
        body(&call, ctor, plan, "        "),
    )
}

fn getter(accessor: &Accessor, plan: &BindingPlan) -> String {
    let field = &accessor.field;
    format!(
        "{}    #[wasm_bindgen(getter{})]\n    pub fn {field}(&self) -> {} {{\n        \
         {}\n    }}\n",
        docs(accessor.doc.as_deref(), "    "),
        rename(field).map(|r| format!(", {r}")).unwrap_or_default(),
        signature_ty(&accessor.ty),
        out(&format!("self.0.{field}.clone()"), &accessor.ty, plan),
    )
}

fn setter(accessor: &Accessor, plan: &BindingPlan) -> String {
    let field = &accessor.field;
    format!(
        "    #[wasm_bindgen(setter{})]\n    pub fn set_{field}(&mut self, value: {}) {{\n        \
         self.0.{field} = {};\n    }}\n",
        rename(field).map(|r| format!(", {r}")).unwrap_or_default(),
        signature_ty(&accessor.ty),
        into(&accessor.ty, plan),
    )
}

fn member(method: &Function, plan: &BindingPlan) -> String {
    let receiver = match method.receiver {
        Receiver::Exclusive => "&mut self",
        _ => "&self",
    };
    let call = format!("self.0.{}({})", method.name, call_args(method, plan));
    format!(
        "{}{}    pub {}fn {}({}) -> {} {{\n{}    }}\n",
        docs(method.doc.as_deref(), "    "),
        attr_line(&method.name),
        asyncness(method),
        method.name,
        join(receiver, &params(method, plan)),
        return_ty(method),
        body(&call, method, plan, "        "),
    )
}

fn associated_fn(function: &Function, krate: &str, plan: &BindingPlan) -> String {
    let call = format!(
        "{}::{}({})",
        inner_path(owner_path(&function.rust_path), krate),
        function.name,
        call_args(function, plan)
    );
    format!(
        "{}{}    pub {}fn {}({}) -> {} {{\n{}    }}\n",
        docs(function.doc.as_deref(), "    "),
        attr_line(&function.name),
        asyncness(function),
        function.name,
        params(function, plan),
        return_ty(function),
        body(&call, function, plan, "        "),
    )
}

fn free_fn(function: &Function, krate: &str, plan: &BindingPlan) -> String {
    let call = format!(
        "{}({})",
        inner_path(&function.rust_path, krate),
        call_args(function, plan)
    );
    format!(
        "\n{}#[wasm_bindgen{}]\npub {}fn {}({}) -> {} {{\n{}}}\n",
        docs(function.doc.as_deref(), ""),
        rename(&function.name)
            .map(|r| format!("({r})"))
            .unwrap_or_default(),
        asyncness(function),
        function.name,
        params(function, plan),
        return_ty(function),
        body(&call, function, plan, "    "),
    )
}

/// wasm-bindgen exports a Rust name as written, so anything whose JavaScript
/// spelling differs carries an explicit rename.
fn rename(rust: &str) -> Option<String> {
    let js = js_ident(rust);
    (js != rust).then(|| format!("js_name = {js}"))
}

/// The call, converted into whatever the generated signature promised: a
/// handle constructor, a mirrored value, a `?` through the error newtype.
fn body(call: &str, function: &Function, plan: &BindingPlan, indent: &str) -> String {
    let call = &match function.is_async {
        true => format!("{call}.await"),
        false => call.to_string(),
    };
    match &function.throws {
        Some(err) => {
            let raised = format!("{call}.map_err({})?", error_name(err));
            format!("{indent}Ok({})\n", out(&raised, &function.ret, plan))
        }
        None => format!("{indent}{}\n", out(call, &function.ret, plan)),
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
        _ => expr.to_string(),
    }
}

fn params(function: &Function, plan: &BindingPlan) -> String {
    function
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, passing(p, plan).0))
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
/// wasm-bindgen takes a borrowed slice or `&str` directly, which spares the
/// `Vec` an owned parameter would allocate and free on every call.
fn passing(param: &Param, plan: &BindingPlan) -> (String, String) {
    let name = &param.name;
    let class_of = |ty: &Ty| match ty {
        Ty::Class(c) => c.clone(),
        _ => String::new(),
    };
    // A borrowed slice is still copied into linear memory, so the win here
    // is only the `Vec` an owned parameter would allocate and free.
    match Transfer::of(param, plan) {
        Transfer::Handle { mirrored: true, .. } => (class_of(&param.ty), format!("{name}.into()")),
        Transfer::Handle { writable: true, .. } => (
            format!("&mut {}", class_of(&param.ty)),
            format!("&mut {name}.0"),
        ),
        Transfer::Handle { .. } => (format!("&{}", class_of(&param.ty)), format!("&{name}.0")),
        Transfer::Text { borrowed: true } => ("&str".into(), name.clone()),
        Transfer::Buffer {
            element,
            borrowed: true,
            writable,
        } => {
            let mutability = if writable { "mut " } else { "" };
            (
                format!("&{mutability}[{}]", signature_ty(&element)),
                name.clone(),
            )
        }
        _ => match param.ownership {
            Ownership::Owned => (signature_ty(&param.ty), name.clone()),
            Ownership::Borrowed => (signature_ty(&param.ty), format!("&{name}")),
            Ownership::BorrowedMut => (signature_ty(&param.ty), format!("&mut {name}")),
        },
    }
}

fn return_ty(function: &Function) -> String {
    let ok = signature_ty(&function.ret);
    match function.throws {
        Some(_) => format!("Result<{ok}, JsValue>"),
        None => ok,
    }
}

/// A type as it appears in generated signatures, where an exported type is
/// its wrapper rather than the Rust type the wrapper holds.
fn signature_ty(ty: &Ty) -> String {
    match ty {
        Ty::Str => "String".into(),
        Ty::Bytes => "Vec<u8>".into(),
        Ty::List(inner) => format!("Vec<{}>", signature_ty(inner)),
        Ty::Optional(inner) => format!("Option<{}>", signature_ty(inner)),
        Ty::Class(name) => name.clone(),
        Ty::Opaque(path) => format!("::{path}"),
        other => other.render(),
    }
}

/// A standalone rename attribute for a member inside a bound `impl`.
fn attr_line(rust: &str) -> String {
    rename(rust)
        .map(|r| format!("    #[wasm_bindgen({r})]\n"))
        .unwrap_or_default()
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
