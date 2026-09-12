//! The glue crate's `src/lib.rs`.
//!
//! Every exported type becomes a locally defined wrapper: `#[napi]` expands
//! to trait impls the orphan rule only permits in the crate that defines the
//! type. The same rule is why a failing call throws through a local error
//! newtype rather than `impl From<UserError> for napi::Error`.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::model::{Ownership, Param, Receiver, Ty};
use crate::naming::ident_part;
use crate::plan::{Accessor, BindingPlan, Class, Function, Transfer};
use crate::{BindOptions, GENERATED_RS, GLUE_ALLOW};

use super::js_ident;

/// Freestanding functions every glue crate with a 64-bit integer needs: a
/// `BigInt` outside the target range fails the call rather than silently
/// truncating, the way a JavaScript number already would if it tried.
const BIGINT_HELPERS: &str = "
fn bigint_to_i64(value: ::napi::bindgen_prelude::BigInt, name: &'static str) -> ::napi::Result<i64> {
    let (value, lossless) = value.get_i64();
    match lossless {
        true => Ok(value),
        false => Err(::napi::Error::from_reason(format!(\"`{name}` does not fit in a 64-bit signed integer\"))),
    }
}

fn bigint_to_u64(value: ::napi::bindgen_prelude::BigInt, name: &'static str) -> ::napi::Result<u64> {
    let (negative, value, lossless) = value.get_u64();
    match negative || !lossless {
        true => Err(::napi::Error::from_reason(format!(\"`{name}` does not fit in a 64-bit unsigned integer\"))),
        false => Ok(value),
    }
}
";

/// Render the glue crate body.
pub(crate) fn render(plan: &BindingPlan, opts: &BindOptions) -> String {
    let krate = format!("::{}", opts.crate_name);
    let mut out = String::from(GENERATED_RS);
    out.push_str(GLUE_ALLOW);
    out.push_str("\nuse napi::bindgen_prelude::*;\nuse napi_derive::napi;\n");
    if needs_bigint_helpers(plan) {
        out.push_str(BIGINT_HELPERS);
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

impl ::std::convert::From<{name}> for ::napi::Error {{
    fn from(err: {name}) -> ::napi::Error {{
        ::napi::Error::from_reason(::std::string::ToString::to_string(&err.0))
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
        "\n{}#[napi]\npub struct {}({});\n",
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
            "\n#[napi]\nimpl {} {{\n{}}}\n",
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
{}#[napi]
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
    let ret = match is_fallible(ctor, plan) {
        true => "Result<Self>",
        false => "Self",
    };
    format!(
        "{}    #[napi(constructor)]\n    pub fn new({}) -> {ret} {{\n{}    }}\n",
        docs(ctor.doc.as_deref(), "    "),
        params(ctor, plan),
        body(&call, ctor, plan, "        "),
    )
}

fn getter(accessor: &Accessor, plan: &BindingPlan) -> String {
    let field = &accessor.field;
    format!(
        "{}    #[napi(getter{})]\n    pub fn {field}(&self) -> {} {{\n        {}\n    }}\n",
        docs(accessor.doc.as_deref(), "    "),
        rename(field).map(|r| format!(", {r}")).unwrap_or_default(),
        signature_ty(&accessor.ty),
        out(&format!("self.0.{field}.clone()"), &accessor.ty, plan),
    )
}

fn setter(accessor: &Accessor, plan: &BindingPlan) -> String {
    let field = &accessor.field;
    let rename = rename(field).map(|r| format!(", {r}")).unwrap_or_default();
    let ty = signature_ty(&accessor.ty);
    let (prelude, arg) = convert_scalar("value", &accessor.ty, plan);
    match prelude {
        Some(p) => format!(
            "    #[napi(setter{rename})]\n    pub fn set_{field}(&mut self, value: {ty}) -> Result<()> {{\n        {p}\n        self.0.{field} = {arg};\n        Ok(())\n    }}\n"
        ),
        None => format!(
            "    #[napi(setter{rename})]\n    pub fn set_{field}(&mut self, value: {ty}) {{\n        self.0.{field} = {arg};\n    }}\n"
        ),
    }
}

fn member(method: &Function, plan: &BindingPlan) -> String {
    let receiver = match method.receiver {
        Receiver::Exclusive => "&mut self",
        _ => "&self",
    };
    let call = format!("self.0.{}({})", method.name, call_args(method, plan));
    format!(
        "{}{}    pub fn {}({}) -> {} {{\n{}    }}\n",
        docs(method.doc.as_deref(), "    "),
        napi_attr(&method.name, "    "),
        method.name,
        join(receiver, &params(method, plan)),
        return_ty(method, plan),
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
        "{}{}    pub fn {}({}) -> {} {{\n{}    }}\n",
        docs(function.doc.as_deref(), "    "),
        napi_attr(&function.name, "    "),
        function.name,
        params(function, plan),
        return_ty(function, plan),
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
        "\n{}{}pub fn {}({}) -> {} {{\n{}}}\n",
        docs(function.doc.as_deref(), ""),
        napi_attr(&function.name, ""),
        function.name,
        params(function, plan),
        return_ty(function, plan),
        body(&call, function, plan, "    "),
    )
}

/// napi-rs exports a Rust name as written, so anything whose JavaScript
/// spelling differs carries an explicit rename.
fn rename(rust: &str) -> Option<String> {
    let js = js_ident(rust);
    (js != rust).then(|| format!("js_name = \"{js}\""))
}

/// A standalone `#[napi]` attribute, carrying a rename when the JavaScript
/// spelling differs from the Rust one. Every napi item needs the attribute
/// itself, unlike wasm-bindgen and pyo3 where one blanket impl covers the
/// whole block.
fn napi_attr(rust: &str, indent: &str) -> String {
    match rename(rust) {
        Some(r) => format!("{indent}#[napi({r})]\n"),
        None => format!("{indent}#[napi]\n"),
    }
}

/// The call, converted into whatever the generated signature promised: a
/// handle constructor, a mirrored value, a `?` through the error newtype.
///
/// A parameter prelude runs first: a `BigInt` outside range fails there,
/// before the underlying call, which is also why a call that cannot itself
/// fail still needs `Ok(..)` once one of its parameters can.
fn body(call: &str, function: &Function, plan: &BindingPlan, indent: &str) -> String {
    let prelude = preludes(function, plan, indent);
    let inner = match &function.throws {
        Some(err) => {
            let raised = format!("{call}.map_err({})?", error_name(err));
            format!("{indent}Ok({})\n", out(&raised, &function.ret, plan))
        }
        None if is_fallible(function, plan) => {
            format!("{indent}Ok({})\n", out(call, &function.ret, plan))
        }
        None => format!("{indent}{}\n", out(call, &function.ret, plan)),
    };
    format!("{prelude}{inner}")
}

/// Whether this call can fail: either the crate itself can raise, or one of
/// its parameters is a `BigInt` that might not fit the width it claims.
fn is_fallible(function: &Function, plan: &BindingPlan) -> bool {
    function.throws.is_some()
        || function
            .params
            .iter()
            .any(|p| is_bigint_ty(&p.ty) && matches!(Transfer::of(p, plan), Transfer::Scalar))
}

/// Whether any 64-bit integer crosses this plan, as a parameter or a field
/// a setter takes, so the conversion helpers are worth emitting at all.
fn needs_bigint_helpers(plan: &BindingPlan) -> bool {
    let params = plan
        .functions()
        .flat_map(|f| f.params.iter())
        .any(|p| is_bigint_ty(&p.ty));
    let fields = plan
        .classes
        .iter()
        .flat_map(|c| c.accessors.iter())
        .any(|a| is_bigint_ty(&a.ty));
    params || fields
}

fn is_bigint_ty(ty: &Ty) -> bool {
    matches!(ty, Ty::I64 | Ty::U64 | Ty::ISize | Ty::USize)
}

/// The statements a call's parameters need before it runs, one `let ... ?;`
/// per fallible conversion.
fn preludes(function: &Function, plan: &BindingPlan, indent: &str) -> String {
    function
        .params
        .iter()
        .filter_map(|p| passing(p, plan).1)
        .map(|line| format!("{indent}{line}\n"))
        .collect()
}

/// A value going the other way, back into the type the user's crate holds:
/// a plain expression, or one that needs a `let ... ?;` prelude first
/// because a `BigInt` outside range cannot silently become one.
fn convert_scalar(name: &str, ty: &Ty, plan: &BindingPlan) -> (Option<String>, String) {
    match ty {
        Ty::Class(class) if plan.is_mirrored(class) => (None, format!("{name}.into()")),
        Ty::I64 => (
            Some(format!("let {name} = bigint_to_i64({name}, \"{name}\")?;")),
            name.to_string(),
        ),
        Ty::U64 => (
            Some(format!("let {name} = bigint_to_u64({name}, \"{name}\")?;")),
            name.to_string(),
        ),
        Ty::ISize => (
            Some(format!(
                "let {name} = bigint_to_i64({name}, \"{name}\")? as isize;"
            )),
            name.to_string(),
        ),
        Ty::USize => (
            Some(format!(
                "let {name} = bigint_to_u64({name}, \"{name}\")? as usize;"
            )),
            name.to_string(),
        ),
        Ty::Bytes => (None, format!("{name}.to_vec()")),
        _ => (None, name.to_string()),
    }
}

/// A value leaving the user's crate, spelled the way the signature promised.
fn out(expr: &str, ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Class(name) if plan.is_mirrored(name) => format!("{expr}.into()"),
        Ty::Class(name) => format!("{name}({expr})"),
        Ty::Bytes => format!("Buffer::from({expr})"),
        Ty::I64 | Ty::U64 => format!("BigInt::from({expr})"),
        Ty::ISize => format!("BigInt::from({expr} as i64)"),
        Ty::USize => format!("BigInt::from({expr} as u64)"),
        Ty::List(inner) => match array_ty(inner) {
            Some(arr) => format!("{arr}::from({expr})"),
            None => expr.to_string(),
        },
        Ty::Optional(inner) => match &**inner {
            Ty::Bytes => format!("{expr}.map(Buffer::from)"),
            Ty::List(elem) if array_ty(elem).is_some() => {
                format!("{expr}.map({}::from)", array_ty(elem).expect("checked"))
            }
            _ => expr.to_string(),
        },
        _ => expr.to_string(),
    }
}

fn params(function: &Function, plan: &BindingPlan) -> String {
    function
        .params
        .iter()
        .map(|p| {
            let binding = match needs_mut(p, plan) {
                true => format!("mut {}", p.name),
                false => p.name.clone(),
            };
            format!("{binding}: {}", passing(p, plan).0)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn call_args(function: &Function, plan: &BindingPlan) -> String {
    function
        .params
        .iter()
        .map(|p| passing(p, plan).2)
        .collect::<Vec<_>>()
        .join(", ")
}

/// How one parameter is spelled, what it needs done before the call, and how
/// it is handed on, decided together so the signature and the call cannot
/// disagree.
///
/// A borrowed buffer arrives as a napi typed array, a view into V8's own
/// memory for the duration of the call, so reading it costs nothing and
/// writing through it needs no copy back.
fn passing(param: &Param, plan: &BindingPlan) -> (String, Option<String>, String) {
    let name = &param.name;
    let class_of = |ty: &Ty| match ty {
        Ty::Class(c) => c.clone(),
        _ => String::new(),
    };
    match Transfer::of(param, plan) {
        Transfer::Handle { mirrored: true, .. } => {
            (class_of(&param.ty), None, format!("{name}.into()"))
        }
        // A napi class instance reaches another call as a `ClassInstance`,
        // never a bare reference: the value lives behind a JS object that
        // may still be aliased elsewhere.
        Transfer::Handle { .. } => (
            format!("ClassInstance<'_, {}>", class_of(&param.ty)),
            None,
            format!("&{name}.0"),
        ),
        Transfer::Text { .. } => ("String".into(), None, name.clone()),
        Transfer::Buffer {
            ref element,
            borrowed,
            writable,
        } if buffer_ty(element).is_some() => {
            let ty = buffer_ty(element).expect("checked");
            let handoff = match (borrowed, writable) {
                (_, true) => format!("unsafe {{ {name}.as_mut() }}"),
                (true, false) => format!("{name}.as_ref()"),
                (false, false) => format!("{name}.to_vec()"),
            };
            (ty.to_string(), None, handoff)
        }
        _ if is_bigint_ty(&param.ty) => {
            let (prelude, arg) = convert_scalar(name, &param.ty, plan);
            ("BigInt".into(), prelude, arg)
        }
        _ => match param.ownership {
            Ownership::Owned => (signature_ty(&param.ty), None, name.clone()),
            Ownership::Borrowed => (signature_ty(&param.ty), None, format!("&{name}")),
            Ownership::BorrowedMut => (signature_ty(&param.ty), None, format!("&mut {name}")),
        },
    }
}

/// A writable buffer is taken through `&mut self`, so its binding is `mut`.
fn needs_mut(param: &Param, plan: &BindingPlan) -> bool {
    matches!(
        Transfer::of(param, plan),
        Transfer::Buffer { writable: true, .. }
    )
}

/// The napi wrapper type a run of `element` crosses through, borrowed or
/// owned: `Buffer` for bytes, the matching typed array for every other
/// primitive napi carries one for.
fn buffer_ty(element: &Ty) -> Option<&'static str> {
    match element {
        Ty::U8 => Some("Buffer"),
        other => array_ty(other),
    }
}

/// The typed array a sequence of one primitive comes back as, or `None` for
/// a type with no such array.
fn array_ty(ty: &Ty) -> Option<&'static str> {
    match ty {
        Ty::I8 => Some("Int8Array"),
        Ty::I16 => Some("Int16Array"),
        Ty::U16 => Some("Uint16Array"),
        Ty::I32 => Some("Int32Array"),
        Ty::U32 => Some("Uint32Array"),
        Ty::F32 => Some("Float32Array"),
        Ty::F64 => Some("Float64Array"),
        Ty::I64 => Some("BigInt64Array"),
        Ty::U64 => Some("BigUint64Array"),
        _ => None,
    }
}

fn return_ty(function: &Function, plan: &BindingPlan) -> String {
    let ok = signature_ty(&function.ret);
    match is_fallible(function, plan) {
        true => format!("Result<{ok}>"),
        false => ok,
    }
}

/// A type as it appears in generated signatures, where an exported type is
/// its wrapper rather than the Rust type the wrapper holds, and a 64-bit
/// integer is the `BigInt` every other backend's numeric choice has to agree
/// with.
fn signature_ty(ty: &Ty) -> String {
    match ty {
        Ty::Str => "String".into(),
        Ty::Bytes => "Buffer".into(),
        Ty::I64 | Ty::U64 | Ty::ISize | Ty::USize => "BigInt".into(),
        Ty::List(inner) => match array_ty(inner) {
            Some(arr) => arr.into(),
            None => format!("Vec<{}>", signature_ty(inner)),
        },
        Ty::Optional(inner) => format!("Option<{}>", signature_ty(inner)),
        Ty::Class(name) => name.clone(),
        Ty::Opaque(path) => format!("::{path}"),
        other => other.render(),
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
