//! `src/rust/src/lib.rs`: `#[extendr]` over local newtypes.
//!
//! extendr already marshals a scalar, a string, and a numeric or raw
//! sequence; the shim only has work to do where R has no exact type at all
//! (a 64-bit integer, a sequence of one), where a plain enum has to cross as
//! a validated string instead of a mirrored value, or where a shape needs a
//! hand-built `Robj` (an `Option` of anything but a plain scalar).

use std::fmt::Write;

use crate::model::{Param, Receiver, Ty};
use crate::plan::{Accessor, BindingPlan, Class, Function, Transfer};
use crate::{BindOptions, GENERATED_RS, GLUE_ALLOW};

use super::types;

/// Converts an R double into `T`, failing when it carries a fraction or
/// falls outside `T`'s range. R has no 64-bit integer, so this is the one
/// place a value crossing as one gets checked.
const CHECKED_INT: &str = "
fn __checked_int<T: ::std::convert::TryFrom<i128>>(
    value: f64,
    name: &str,
) -> ::std::result::Result<T, String> {
    if !value.is_finite() || value.fract() != 0.0 {
        return Err(format!(\"`{name}` is not a whole number\"));
    }
    T::try_from(value as i128).map_err(|_| format!(\"`{name}` is out of range\"))
}
";

/// Render the glue crate body.
pub(crate) fn render(plan: &BindingPlan, opts: &BindOptions) -> String {
    let krate = format!("::{}", opts.crate_name);
    let mut out = String::from(GENERATED_RS);
    out.push_str(GLUE_ALLOW);
    out.push_str("\nuse extendr_api::prelude::*;\n");
    if needs_checked_int(plan) {
        out.push_str(CHECKED_INT);
    }
    for class in plan.classes.iter().filter(|c| !c.is_plain_enum()) {
        out.push_str(&class_block(class, &krate, plan));
    }
    for function in &plan.functions {
        out.push_str(&shim(function, None, &krate, plan));
    }
    out.push_str(&module_block(plan, &types::lib_name(&opts.package)));
    out
}

/// Whether any parameter anywhere needs `__checked_int`. A return value only
/// ever widens to `f64`, which cannot fail, so only a parameter or a buffer
/// element is ever checked.
fn needs_checked_int(plan: &BindingPlan) -> bool {
    plan.functions()
        .flat_map(|f| f.params.iter())
        .any(|p| param_ty_needs_checked_int(&p.ty))
}

fn param_ty_needs_checked_int(ty: &Ty) -> bool {
    types::checked_int(ty).is_some()
        || matches!(ty, Ty::List(inner) if types::checked_int(inner).is_some())
}

/// A local newtype over an exported type, plus one method per bound call.
/// extendr's own `#[extendr] struct` derive gives the wrapper its external
/// pointer shape and its S3 class; nothing here spells either by hand.
fn class_block(class: &Class, krate: &str, plan: &BindingPlan) -> String {
    let inner = inner_path(&class.rust_path, krate);
    let name = &class.name;
    let mut out = format!("\n#[extendr]\nstruct {name}({inner});\n\n#[extendr]\nimpl {name} {{\n");
    if let Some(ctor) = &class.ctor {
        out.push_str(&method(ctor, Some(class), krate, plan));
    }
    for accessor in &class.accessors {
        out.push_str(&getter(accessor, krate, plan));
    }
    for function in class.methods.iter().chain(class.statics.iter()) {
        out.push_str(&method(function, Some(class), krate, plan));
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out.push_str("}\n");
    out
}

/// A field read. An exported type held by a field never reaches here,
/// because the plan reports it instead, the same as every other backend.
fn getter(accessor: &Accessor, krate: &str, plan: &BindingPlan) -> String {
    let name = &accessor.field;
    let ret = ret_ty(&accessor.ty, plan);
    let read = returned(&format!("self.0.{name}.clone()"), &accessor.ty, krate, plan);
    format!("    fn {name}(&self) -> {ret} {{\n        {read}\n    }}\n\n")
}

/// One bound call, as a method on the local newtype. `body` renders at the
/// indentation a free function's own body would use; nested one level
/// deeper inside `impl`, it needs shifting right by one more level.
fn method(function: &Function, owner: Option<&Class>, krate: &str, plan: &BindingPlan) -> String {
    let fallible = is_fallible(function, plan);
    let sig = signature(function, owner, plan, fallible);
    let body = indent(&body(function, owner, krate, plan, fallible));
    format!("    fn {sig} {{\n{body}    }}\n\n")
}

/// Shifts every line of a rendered body one level to the right.
fn indent(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if !line.is_empty() {
            out.push_str("    ");
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// A free function: the same shape as a method, minus a receiver.
fn shim(function: &Function, owner: Option<&Class>, krate: &str, plan: &BindingPlan) -> String {
    let fallible = is_fallible(function, plan);
    let sig = signature(function, owner, plan, fallible);
    let body = body(function, owner, krate, plan, fallible);
    format!("\n#[extendr]\nfn {sig} {{\n{body}}}\n")
}

/// Whether converting a parameter or validating an enum can fail even when
/// the user's own function cannot: a 64-bit integer or an enum crossing as a
/// string both get checked on the way in, and a check that can fail has to
/// make the whole call fallible.
fn is_fallible(function: &Function, plan: &BindingPlan) -> bool {
    function.throws.is_some()
        || function.params.iter().any(|p| match Transfer::of(p, plan) {
            Transfer::Handle { mirrored: true, .. } => true,
            _ => param_ty_needs_checked_int(&p.ty),
        })
}

fn signature(
    function: &Function,
    owner: Option<&Class>,
    plan: &BindingPlan,
    fallible: bool,
) -> String {
    let mut params = Vec::new();
    if owner.is_some() && function.receiver != Receiver::None {
        params.push(receiver_param(function.receiver));
    }
    for param in &function.params {
        params.push(format!("{}: {}", param.name, param_ty(param, plan)));
    }
    let ret = ret_clause(function, plan, fallible);
    format!("{}({}){ret}", function.name, params.join(", "))
}

fn receiver_param(receiver: Receiver) -> String {
    match receiver {
        Receiver::Exclusive => "&mut self".into(),
        _ => "&self".into(),
    }
}

fn ret_clause(function: &Function, plan: &BindingPlan, fallible: bool) -> String {
    let inner = ret_ty(&function.ret, plan);
    match (fallible, inner.is_empty()) {
        (false, true) => String::new(),
        (false, false) => format!(" -> {inner}"),
        (true, true) => " -> ::std::result::Result<(), String>".into(),
        (true, false) => format!(" -> ::std::result::Result<{inner}, String>"),
    }
}

/// A parameter's extendr-facing Rust type.
fn param_ty(param: &Param, plan: &BindingPlan) -> String {
    match Transfer::of(param, plan) {
        Transfer::Text { borrowed: true } => "&str".into(),
        Transfer::Text { borrowed: false } => "String".into(),
        Transfer::Handle { mirrored: true, .. } => "&str".into(),
        Transfer::Handle { writable, .. } => {
            let name = class_name(&param.ty);
            match writable {
                true => format!("&mut {name}"),
                false => format!("&{name}"),
            }
        }
        Transfer::Buffer { element, .. } => buffer_param_ty(&element),
        _ => scalar_ty(&param.ty),
    }
}

fn scalar_ty(ty: &Ty) -> String {
    types::checked_int(ty)
        .map(|_| "f64".to_string())
        .or_else(|| types::native_scalar(ty).map(str::to_string))
        .unwrap_or_default()
}

fn buffer_param_ty(element: &Ty) -> String {
    let native = types::buffer_native(element).unwrap_or("f64");
    match types::checked_int(element) {
        Some(_) => "&[f64]".into(),
        None => format!("&[{native}]"),
    }
}

/// A return type's extendr-facing Rust spelling, ignoring fallibility (the
/// `Result` wrapper, when one is needed, is added by [`ret_clause`]).
fn ret_ty(ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Unit => String::new(),
        Ty::Str => "String".into(),
        Ty::Class(name) if plan.is_mirrored(name) => "String".into(),
        Ty::Class(name) => name.clone(),
        Ty::Optional(inner) if types::option_native(inner) => {
            format!("Option<{}>", option_inner_ty(inner))
        }
        Ty::Optional(_) => "Robj".into(),
        Ty::Bytes => "Vec<u8>".into(),
        Ty::List(inner) => format!("Vec<{}>", buffer_ret_element(inner)),
        ty => scalar_ty(ty),
    }
}

fn option_inner_ty(ty: &Ty) -> String {
    match ty {
        Ty::Str => "String".into(),
        ty => types::native_scalar(ty).unwrap_or_default().into(),
    }
}

fn buffer_ret_element(ty: &Ty) -> &'static str {
    match types::checked_int(ty) {
        Some(_) => "f64",
        None => types::buffer_native(ty).unwrap_or("f64"),
    }
}

/// The function body: convert every parameter that needs it, make the call,
/// and shape whatever it returns into the type the signature promised.
fn body(
    function: &Function,
    owner: Option<&Class>,
    krate: &str,
    plan: &BindingPlan,
    fallible: bool,
) -> String {
    let mut out = String::new();
    for param in &function.params {
        out.push_str(&param_prelude(param, krate, plan));
    }
    let call = call_expr(function, owner, krate, plan);
    if function.ret == Ty::Unit && function.throws.is_none() {
        let _ = writeln!(out, "    {call};");
        if fallible {
            out.push_str("    Ok(())\n");
        }
        return out;
    }
    match &function.throws {
        None => {
            let _ = writeln!(out, "    let __out = {call};");
            let ret = returned("__out", &function.ret, krate, plan);
            let _ = writeln!(out, "    {}", wrap_ok(&ret, fallible));
        }
        Some(_) => {
            let ok = returned("value", &function.ret, krate, plan);
            let _ = writeln!(out, "    match {call} {{");
            let _ = writeln!(out, "        Ok(value) => Ok({ok}),");
            let _ = writeln!(
                out,
                "        Err(reason) => Err(::std::string::ToString::to_string(&reason)),"
            );
            let _ = writeln!(out, "    }}");
        }
    }
    out
}

fn wrap_ok(expr: &str, fallible: bool) -> String {
    match fallible {
        true => format!("Ok({expr})"),
        false => expr.to_string(),
    }
}

/// The statement converting one parameter into the shape the real call
/// needs, or nothing for a parameter that already arrives in it.
fn param_prelude(param: &Param, krate: &str, plan: &BindingPlan) -> String {
    let name = &param.name;
    match Transfer::of(param, plan) {
        Transfer::Handle { mirrored: true, .. } => enum_from_str(param, krate, plan),
        Transfer::Buffer {
            element, borrowed, ..
        } => buffer_prelude(name, &element, borrowed),
        _ => match types::checked_int(&param.ty) {
            Some(rust) => {
                format!("    let {name} = __checked_int::<{rust}>({name}, \"{name}\")?;\n")
            }
            None => String::new(),
        },
    }
}

/// A checked-int element always needs a fresh, converted `Vec`, whatever the
/// real function wants; a native one is already the right element type and
/// only needs copying out of R's own memory when the call takes it owned.
fn buffer_prelude(name: &str, element: &Ty, borrowed: bool) -> String {
    match types::checked_int(element) {
        Some(rust) => format!(
            "    let {name}: Vec<{rust}> = {name}\n        .iter()\n        .map(|v| __checked_int::<{rust}>(*v, \"{name}\"))\n        .collect::<::std::result::Result<_, _>>()?;\n"
        ),
        None if borrowed => String::new(),
        None => format!("    let {name} = {name}.to_vec();\n"),
    }
}

/// A plain enum crossing as a string, validated against its own variant
/// names: unlike a mirrored ordinal, this reads the same in an R traceback
/// as the value a caller actually passed.
fn enum_from_str(param: &Param, krate: &str, plan: &BindingPlan) -> String {
    let name = &param.name;
    let class_name = class_name(&param.ty);
    let class = plan
        .classes
        .iter()
        .find(|c| c.name == class_name)
        .expect("mirrored handle names a known class");
    let inner = inner_path(&class.rust_path, krate);
    let mut arms = String::new();
    for variant in class.variants.iter().flatten() {
        let _ = writeln!(arms, "            \"{0}\" => Inner::{0},", variant.name);
    }
    format!(
        "    let {name} = {{\n        type Inner = {inner};\n        match {name} {{\n{arms}            other => return Err(format!(\"unknown {class_name} variant: {{other}}\", other = other)),\n        }}\n    }};\n"
    )
}

fn returned(expr: &str, ty: &Ty, krate: &str, plan: &BindingPlan) -> String {
    match ty {
        Ty::Unit => expr.to_string(),
        Ty::Class(name) if plan.is_mirrored(name) => enum_to_str(expr, name, krate, plan),
        Ty::Class(name) => format!("{name}({expr})"),
        Ty::Optional(inner) if types::option_native(inner) => expr.to_string(),
        Ty::Optional(inner) => optional_to_robj(expr, inner, krate, plan),
        ty if types::checked_int(ty).is_some() => format!("{expr} as f64"),
        Ty::List(inner) if types::checked_int(inner).is_some() => {
            format!("{expr}.into_iter().map(|v| v as f64).collect()")
        }
        _ => expr.to_string(),
    }
}

fn enum_to_str(expr: &str, name: &str, krate: &str, plan: &BindingPlan) -> String {
    let class = plan
        .classes
        .iter()
        .find(|c| c.name == name)
        .expect("mirrored return names a known class");
    let inner = inner_path(&class.rust_path, krate);
    let mut arms = String::new();
    for variant in class.variants.iter().flatten() {
        let _ = writeln!(
            arms,
            "        {inner}::{0} => \"{0}\".to_string(),",
            variant.name
        );
    }
    format!("match {expr} {{\n{arms}    }}")
}

fn optional_to_robj(expr: &str, inner: &Ty, krate: &str, plan: &BindingPlan) -> String {
    let some = returned("v", inner, krate, plan);
    format!("match {expr} {{ Some(v) => Robj::from({some}), None => ().into() }}")
}

fn call_expr(
    function: &Function,
    owner: Option<&Class>,
    krate: &str,
    plan: &BindingPlan,
) -> String {
    let args: Vec<String> = function.params.iter().map(|p| call_arg(p, plan)).collect();
    let args = args.join(", ");
    match (owner, function.receiver) {
        (Some(_), Receiver::None) => format!(
            "{}::{}({args})",
            inner_path(owner_path(&function.rust_path), krate),
            function.name,
        ),
        (Some(_), _) => format!("self.0.{}({args})", function.name),
        (None, _) => format!("{}({args})", inner_path(&function.rust_path, krate)),
    }
}

fn call_arg(param: &Param, plan: &BindingPlan) -> String {
    let name = &param.name;
    match Transfer::of(param, plan) {
        Transfer::Handle { mirrored: true, .. } => name.clone(),
        Transfer::Handle { writable, .. } => match writable {
            true => format!("&mut {name}.0"),
            false => format!("&{name}.0"),
        },
        // A checked-int buffer is always converted into a fresh, owned
        // `Vec`; a call that wants it borrowed still needs the reference.
        Transfer::Buffer {
            element, borrowed, ..
        } if borrowed && types::checked_int(&element).is_some() => format!("&{name}"),
        _ => name.clone(),
    }
}

fn class_name(ty: &Ty) -> String {
    match ty {
        Ty::Class(name) => name.clone(),
        Ty::Optional(inner) => class_name(inner),
        _ => String::new(),
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

/// The `extendr_module!` block, naming every item the R side calls through.
fn module_block(plan: &BindingPlan, lib: &str) -> String {
    let mut out = format!("\nextendr_module! {{\n    mod {lib};\n");
    for class in plan.classes.iter().filter(|c| !c.is_plain_enum()) {
        let _ = writeln!(out, "    impl {};", class.name);
    }
    for function in &plan.functions {
        let _ = writeln!(out, "    fn {};", function.name);
    }
    out.push_str("}\n");
    out
}
