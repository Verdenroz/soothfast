//! The glue crate's `ext/<module>/src/lib.rs`.
//!
//! Every exported type becomes a locally defined wrapper, the same orphan-
//! rule reason every other backend defines one: `#[magnus::wrap]` expands
//! to trait impls only the defining crate may write. The wrapper holds a
//! `RefCell` because magnus hands a bound method `&self` on the wrapper
//! however the underlying call needs it; the `RefCell` is what turns that
//! into the `&mut` an exclusive receiver still requires.
//!
//! A plain enum crosses as a Ruby `Symbol` rather than a class of its own,
//! so unlike every other backend it gets no wrapper at all: just the two
//! free functions converting it, checked against the known variant names on
//! the way in since a `Symbol` carries no static guarantee of that.

use std::fmt::Write;

use crate::model::{Ownership, Param, Receiver, Ty};
use crate::plan::{Accessor, BindingPlan, Class, Function, Transfer};
use crate::{BindOptions, GENERATED_RS, GLUE_ALLOW};

use super::rb_ident;
use super::types::{self, signature_ty};

/// Render the glue crate body.
pub(crate) fn render(plan: &BindingPlan, opts: &BindOptions, module: &str) -> String {
    let krate = format!("::{}", opts.crate_name);
    let mut out = String::from(GENERATED_RS);
    out.push_str(GLUE_ALLOW);
    out.push_str("\nuse ::magnus::prelude::*;\n");
    out.push_str(&error_static(module));

    for class in &plan.classes {
        if class.is_plain_enum() {
            out.push_str(&enum_conversions(class, &krate));
        }
    }
    for class in plan.classes.iter().filter(|c| !c.is_plain_enum()) {
        out.push_str(&class_block(class, &krate, plan, module));
    }
    for function in &plan.functions {
        out.push_str(&free_fn(function, &krate, plan));
    }
    out.push_str(&init_fn(plan, module));
    out
}

/// One `Error` class for the whole package, defined lazily the first time
/// anything needs it and forced eagerly at load so `rescue Module::Error`
/// works even before a call ever fails.
fn error_static(module: &str) -> String {
    format!(
        "\nstatic ERROR: ::magnus::value::Lazy<::magnus::ExceptionClass> = \
         ::magnus::value::Lazy::new(|ruby| {{\n    \
         ruby.define_module(\"{module}\")\n        \
         .and_then(|m| m.define_error(\"Error\", ruby.exception_standard_error()))\n        \
         .expect(\"defines {module}::Error\")\n}});\n"
    )
}

/// A payload-free enum crosses as a `Symbol`, validated against its known
/// variant names on the way in; converting one out never fails.
fn enum_conversions(class: &Class, krate: &str) -> String {
    let inner = inner_path(&class.rust_path, krate);
    let name = types::snake(&class.name);
    let names: Vec<&str> = class
        .variants
        .iter()
        .flatten()
        .map(|v| v.name.as_str())
        .collect();
    let mut from_arms = String::new();
    let mut to_arms = String::new();
    for variant in &names {
        let symbol = types::snake(variant);
        let _ = writeln!(from_arms, "        \"{symbol}\" => Ok({inner}::{variant}),");
        let _ = writeln!(
            to_arms,
            "        {inner}::{variant} => ruby.to_symbol(\"{symbol}\"),"
        );
    }
    format!(
        "\nfn {name}_from_symbol(\n    ruby: &::magnus::Ruby,\n    value: ::magnus::Symbol,\n) -> Result<{inner}, ::magnus::Error> {{\n    match value.name()?.as_ref() {{\n{from_arms}        other => Err(::magnus::Error::new(\n            ruby.exception_arg_error(),\n            format!(\"invalid {}: :{{other}}\"),\n        )),\n    }}\n}}\n\nfn {name}_to_symbol(ruby: &::magnus::Ruby, value: {inner}) -> ::magnus::Symbol {{\n    match value {{\n{to_arms}    }}\n}}\n",
        class.name,
    )
}

fn class_block(class: &Class, krate: &str, plan: &BindingPlan, module: &str) -> String {
    let inner = inner_path(&class.rust_path, krate);
    let mut out = format!(
        "\n{}#[::magnus::wrap(class = \"{module}::{}\", free_immediately)]\npub struct {}(::std::cell::RefCell<{inner}>);\n",
        docs(class.doc.as_deref(), ""),
        class.name,
        class.name,
    );

    let mut members = Vec::new();
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
        let _ = write!(out, "\nimpl {} {{\n{}}}\n", class.name, members.join("\n"));
    }
    out
}

/// `ctor.ret` is already `Ty::Class(owner.name)`, the same as any other
/// function returning the type it builds, so `body` wraps it into `Self`
/// through the same [`wrap_return`] path a plain return uses. Wrapping it
/// again here would double the `RefCell`.
fn constructor(ctor: &Function, krate: &str, plan: &BindingPlan) -> String {
    let planned = plan_params(ctor, plan);
    let call = format!(
        "{}::{}({})",
        inner_path(owner_path(&ctor.rust_path), krate),
        ctor.name,
        call_args(&planned)
    );
    let ret = match fallible(ctor, plan) {
        true => "Result<Self, ::magnus::Error>".to_string(),
        false => "Self".to_string(),
    };
    format!(
        "{}    fn new({}) -> {ret} {{\n{}    }}\n",
        docs(ctor.doc.as_deref(), "    "),
        free_params_sig(&planned, ctor, needs_ruby(ctor, plan)),
        body(
            &call,
            ctor,
            plan,
            "        ",
            &preludes(&planned),
            &postludes(&planned)
        ),
    )
}

/// A getter clones, which every bound field type supports: an exported type
/// held by a field never reaches here (the plan reports it instead).
fn getter(accessor: &Accessor, plan: &BindingPlan) -> String {
    let name = rb_ident(&accessor.field);
    let field = &accessor.field;
    let needs_ruby = ty_has_mirrored_enum(&accessor.ty, plan);
    let expr = format!("{}.0.borrow().{field}.clone()", self_var(needs_ruby));
    format!(
        "{}    fn {name}({}) -> {} {{\n        {}\n    }}\n",
        docs(accessor.doc.as_deref(), "    "),
        receiver_sig(needs_ruby, false),
        returned_ty(&accessor.ty, plan),
        wrap_return(&expr, &accessor.ty, plan),
    )
}

fn setter(accessor: &Accessor, plan: &BindingPlan) -> String {
    let name = rb_ident(&accessor.field);
    let field = &accessor.field;
    let mirrored = matches!(&accessor.ty, Ty::Class(class) if plan.is_mirrored(class));
    let (param_ty, prelude, assigned) = match &accessor.ty {
        Ty::Bytes => (
            "::magnus::RString".to_string(),
            Some("let value = unsafe { value.as_slice() }.to_vec();".to_string()),
            "value".to_string(),
        ),
        Ty::Class(class) if plan.is_mirrored(class) => (
            "::magnus::Symbol".to_string(),
            Some(format!(
                "let value = {}_from_symbol(ruby, value)?;",
                types::snake(class)
            )),
            "value".to_string(),
        ),
        ty => (signature_ty(ty), None, "value".to_string()),
    };
    let ret = if mirrored {
        " -> Result<(), ::magnus::Error>"
    } else {
        ""
    };
    let mut out_body = String::new();
    if let Some(stmt) = &prelude {
        let _ = writeln!(out_body, "        {stmt}");
    }
    let _ = writeln!(
        out_body,
        "        {}.0.borrow_mut().{field} = {assigned};",
        self_var(mirrored),
    );
    if mirrored {
        out_body.push_str("        Ok(())\n");
    }
    format!(
        "    fn set_{name}({}, value: {param_ty}){ret} {{\n{out_body}    }}\n",
        receiver_sig(mirrored, false),
    )
}

fn member(method: &Function, plan: &BindingPlan) -> String {
    let planned = plan_params(method, plan);
    let needs_ruby = needs_ruby(method, plan);
    let exclusive = method.receiver == Receiver::Exclusive;
    let call = format!(
        "{}.0.borrow{}().{}({})",
        self_var(needs_ruby),
        if exclusive { "_mut" } else { "" },
        method.name,
        call_args(&planned)
    );
    let ret_ty = wrapped_ret_ty(method, plan);
    format!(
        "{}    fn {}({}) -> {ret_ty} {{\n{}    }}\n",
        docs(method.doc.as_deref(), "    "),
        method.name,
        join(
            &receiver_sig(needs_ruby, exclusive),
            &params_sig(&planned, method)
        ),
        body(
            &call,
            method,
            plan,
            "        ",
            &preludes(&planned),
            &postludes(&planned)
        ),
    )
}

fn associated_fn(function: &Function, krate: &str, plan: &BindingPlan) -> String {
    let planned = plan_params(function, plan);
    let call = format!(
        "{}::{}({})",
        inner_path(owner_path(&function.rust_path), krate),
        function.name,
        call_args(&planned)
    );
    let ret_ty = wrapped_ret_ty(function, plan);
    format!(
        "{}    fn {}({}) -> {ret_ty} {{\n{}    }}\n",
        docs(function.doc.as_deref(), "    "),
        function.name,
        free_params_sig(&planned, function, needs_ruby(function, plan)),
        body(
            &call,
            function,
            plan,
            "        ",
            &preludes(&planned),
            &postludes(&planned)
        ),
    )
}

fn free_fn(function: &Function, krate: &str, plan: &BindingPlan) -> String {
    let planned = plan_params(function, plan);
    let call = format!(
        "{}({})",
        inner_path(&function.rust_path, krate),
        call_args(&planned)
    );
    let ret_ty = wrapped_ret_ty(function, plan);
    format!(
        "\n{}fn {}({}) -> {ret_ty} {{\n{}}}\n",
        docs(function.doc.as_deref(), ""),
        function.name,
        free_params_sig(&planned, function, needs_ruby(function, plan)),
        body(
            &call,
            function,
            plan,
            "    ",
            &preludes(&planned),
            &postludes(&planned)
        ),
    )
}

/// `self.0` or `rb_self.0`, matching whichever [`receiver_sig`] chose: a
/// method needing the `Ruby` handle takes it as a leading parameter, which
/// magnus's `method!` macro reads only from a receiver spelled as a plain
/// `rb_self` binding rather than `self` sugar.
fn self_var(needs_ruby: bool) -> &'static str {
    if needs_ruby { "rb_self" } else { "self" }
}

/// The receiver clause for a bound method: `&self`/`&mut self` normally, or
/// `ruby: &::magnus::Ruby, rb_self: &Self`/`&mut Self` when the body needs
/// the interpreter handle. `method!`/`function!` read a leading `&Ruby`
/// parameter as that handle without it counting toward the declared arity.
fn receiver_sig(needs_ruby: bool, exclusive: bool) -> String {
    match needs_ruby {
        true => format!(
            "ruby: &::magnus::Ruby, rb_self: &{}Self",
            if exclusive { "mut " } else { "" }
        ),
        false => format!("&{}self", if exclusive { "mut " } else { "" }),
    }
}

/// A receiver-less signature (a constructor, a static, or a free function):
/// the same leading `&Ruby` parameter, with no `self` to spell around.
fn free_params_sig(planned: &[PlannedParam], function: &Function, needs_ruby: bool) -> String {
    let lead = needs_ruby.then(|| "ruby: &::magnus::Ruby".to_string());
    join(&lead.unwrap_or_default(), &params_sig(planned, function))
}

fn join(lead: &str, rest: &str) -> String {
    match (lead.is_empty(), rest.is_empty()) {
        (true, _) => rest.to_string(),
        (false, true) => lead.to_string(),
        (false, false) => format!("{lead}, {rest}"),
    }
}

fn wrapped_ret_ty(function: &Function, plan: &BindingPlan) -> String {
    let ok = returned_ty(&function.ret, plan);
    match fallible(function, plan) {
        true => format!("Result<{ok}, ::magnus::Error>"),
        false => ok,
    }
}

/// The call, converted into whatever the generated signature promised: a
/// wrapped handle, a validated symbol, a `?` through the package `Error`.
/// `ruby`, when the body needs it, already reached the signature as
/// [`receiver_sig`]'s or [`free_params_sig`]'s leading parameter.
///
/// A writable buffer parameter needs its write-back to run *after* the call,
/// so a body with postludes binds the call to `__out` first rather than
/// inlining it into the final expression the way a body with none does.
fn body(
    call: &str,
    function: &Function,
    plan: &BindingPlan,
    indent: &str,
    preludes: &[String],
    postludes: &[String],
) -> String {
    let mut out = String::new();
    for prelude in preludes {
        let _ = writeln!(out, "{indent}{prelude}");
    }
    let called = match &function.throws {
        Some(_) => format!(
            "{call}.map_err(|reason| ::magnus::Error::new(ruby.get_inner(&ERROR), ::std::string::ToString::to_string(&reason)))?"
        ),
        None => call.to_string(),
    };
    let result = if postludes.is_empty() {
        called
    } else {
        let _ = writeln!(out, "{indent}let __out = {called};");
        for postlude in postludes {
            let _ = writeln!(out, "{indent}{postlude}");
        }
        "__out".to_string()
    };
    let wrapped = wrap_return(&result, &function.ret, plan);
    match fallible(function, plan) {
        true => {
            let _ = writeln!(out, "{indent}Ok({wrapped})");
        }
        false => {
            let _ = writeln!(out, "{indent}{wrapped}");
        }
    }
    out
}

/// Whether the generated signature has to return `Result`: the call itself
/// can fail, a mirrored-enum parameter can (an unrecognized symbol raises
/// before the call ever runs), and so can converting a writable buffer
/// parameter to and from the caller's `Array`.
fn fallible(function: &Function, plan: &BindingPlan) -> bool {
    function.throws.is_some()
        || function.params.iter().any(|p| match Transfer::of(p, plan) {
            Transfer::Handle { mirrored: true, .. } => true,
            Transfer::Buffer { writable: true, .. } => !matches!(p.ty, Ty::Bytes),
            _ => false,
        })
}

/// Whether the body needs a `Ruby` handle at all: raising the package error,
/// validating a mirrored-enum parameter, and building one to return all do.
/// A writable buffer's own `?`s do not: `RArray::to_vec`/`store` need no
/// handle, so that case alone must not drag in an unused parameter.
fn needs_ruby(function: &Function, plan: &BindingPlan) -> bool {
    function.throws.is_some()
        || function.params.iter().any(|p| {
            matches!(
                Transfer::of(p, plan),
                Transfer::Handle { mirrored: true, .. }
            )
        })
        || ty_has_mirrored_enum(&function.ret, plan)
}

fn ty_has_mirrored_enum(ty: &Ty, plan: &BindingPlan) -> bool {
    match ty {
        Ty::Class(name) => plan.is_mirrored(name),
        Ty::Optional(inner) | Ty::List(inner) => ty_has_mirrored_enum(inner, plan),
        _ => false,
    }
}

/// A value leaving the user's crate, shaped into whatever [`returned_ty`]
/// promised.
fn wrap_return(expr: &str, ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Optional(inner) => match wrap_return("value", inner, plan) {
            // Nothing about the inner type needs shaping, so the `Option` the
            // call already returns needs no `.map` to become the one this
            // signature promises.
            mapped if mapped == "value" => expr.to_string(),
            mapped => format!("({expr}).map(|value| {mapped})"),
        },
        Ty::Class(name) if plan.is_mirrored(name) => {
            format!("{}_to_symbol(ruby, {expr})", types::snake(name))
        }
        Ty::Class(name) => format!("{name}(::std::cell::RefCell::new({expr}))"),
        Ty::Bytes => format!("::magnus::RString::from_slice(&{expr})"),
        _ => expr.to_string(),
    }
}

/// A returned type, where a mirrored enum comes back as a `Symbol` and a
/// byte sequence as a Ruby string rather than an array of small integers.
fn returned_ty(ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Optional(inner) => format!("Option<{}>", returned_ty(inner, plan)),
        Ty::Class(name) if plan.is_mirrored(name) => "::magnus::Symbol".into(),
        Ty::Class(name) => name.clone(),
        Ty::Bytes => "::magnus::RString".into(),
        _ => signature_ty(ty),
    }
}

/// One planned parameter: its generated signature type, an optional
/// conversion statement run before the call, the expression the call itself
/// passes, and an optional write-back statement run after the call.
type PlannedParam = (String, Option<String>, String, Option<String>);

fn plan_params(function: &Function, plan: &BindingPlan) -> Vec<PlannedParam> {
    function
        .params
        .iter()
        .map(|p| param_plan(p, plan))
        .collect()
}

fn params_sig(planned: &[PlannedParam], function: &Function) -> String {
    function
        .params
        .iter()
        .zip(planned)
        .map(|(p, (ty, ..))| format!("{}: {ty}", p.name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn call_args(planned: &[PlannedParam]) -> String {
    planned
        .iter()
        .map(|(_, _, arg, _)| arg.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

fn preludes(planned: &[PlannedParam]) -> Vec<String> {
    planned
        .iter()
        .filter_map(|(_, prelude, ..)| prelude.clone())
        .collect()
}

fn postludes(planned: &[PlannedParam]) -> Vec<String> {
    planned
        .iter()
        .filter_map(|(.., postlude)| postlude.clone())
        .collect()
}

/// How one parameter is spelled and how it is handed on, decided together
/// so the signature and the call cannot disagree.
///
/// A borrowed or owned buffer always converts into an owned `Vec`/`RString`:
/// magnus has nothing like wasm-bindgen's linear-memory view, so there is no
/// borrowed form to reach for here regardless of the Rust signature's own
/// ownership. A *writable* buffer of one primitive is the one shape that
/// still has to look mutated to the caller: it takes the `RArray` itself so
/// the postlude can write the converted `Vec` back into it element by
/// element once the call returns.
fn param_plan(param: &Param, plan: &BindingPlan) -> PlannedParam {
    let name = &param.name;
    match Transfer::of(param, plan) {
        Transfer::Handle { mirrored: true, .. } => {
            let helper = format!("{}_from_symbol", types::snake(&class_name(&param.ty)));
            (
                "::magnus::Symbol".into(),
                Some(format!("let {name} = {helper}(ruby, {name})?;")),
                name.clone(),
                None,
            )
        }
        Transfer::Handle { writable: true, .. } => (
            format!("&{}", class_name(&param.ty)),
            None,
            format!("&mut {name}.0.borrow_mut()"),
            None,
        ),
        Transfer::Handle { .. } => (
            format!("&{}", class_name(&param.ty)),
            None,
            format!("&{name}.0.borrow()"),
            None,
        ),
        Transfer::Buffer { .. } if matches!(param.ty, Ty::Bytes) => (
            "::magnus::RString".into(),
            Some(format!(
                "let {name} = unsafe {{ {name}.as_slice() }}.to_vec();"
            )),
            by_ownership(name, param.ownership),
            None,
        ),
        Transfer::Buffer {
            element,
            writable: true,
            ..
        } => {
            let vec = format!("{name}_vec");
            (
                "::magnus::RArray".into(),
                Some(format!(
                    "let mut {vec} = {name}.to_vec::<{}>()?;",
                    element.render()
                )),
                format!("&mut {vec}"),
                Some(format!(
                    "{vec}.iter().enumerate().try_for_each(|(i, value)| {name}.store(i as isize, *value))?;"
                )),
            )
        }
        Transfer::Buffer { element, .. } => (
            format!("Vec<{}>", element.render()),
            None,
            by_ownership(name, param.ownership),
            None,
        ),
        _ => (
            signature_ty(&param.ty),
            None,
            by_ownership(name, param.ownership),
            None,
        ),
    }
}

fn by_ownership(name: &str, ownership: Ownership) -> String {
    match ownership {
        Ownership::Owned => name.to_string(),
        Ownership::Borrowed => format!("&{name}"),
        Ownership::BorrowedMut => format!("&mut {name}"),
    }
}

fn class_name(ty: &Ty) -> String {
    match ty {
        Ty::Class(name) => name.clone(),
        Ty::Optional(inner) => class_name(inner),
        _ => String::new(),
    }
}

fn init_fn(plan: &BindingPlan, module: &str) -> String {
    let mut body = String::new();
    let _ = writeln!(body, "    let module = ruby.define_module(\"{module}\")?;");
    body.push_str("    ::magnus::value::Lazy::force(&ERROR, ruby);\n");
    for class in plan.classes.iter().filter(|c| !c.is_plain_enum()) {
        body.push_str(&class_registration(class));
    }
    for function in &plan.functions {
        let _ = writeln!(
            body,
            "    module.define_module_function(\"{}\", ::magnus::function!({}, {}))?;",
            rb_ident(&function.name),
            function.name,
            function.params.len(),
        );
    }
    format!(
        "\n#[::magnus::init]\nfn init(ruby: &::magnus::Ruby) -> Result<(), ::magnus::Error> {{\n{body}    Ok(())\n}}\n"
    )
}

fn class_registration(class: &Class) -> String {
    let has_members = class.ctor.is_some()
        || !class.accessors.is_empty()
        || !class.methods.is_empty()
        || !class.statics.is_empty();
    let binding = if has_members { "let class = " } else { "" };
    let mut out = format!(
        "    {binding}module.define_class(\"{}\", ruby.class_object())?;\n",
        class.name
    );
    if let Some(ctor) = &class.ctor {
        let _ = writeln!(
            out,
            "    class.define_singleton_method(\"new\", ::magnus::function!({}::new, {}))?;",
            class.name,
            ctor.params.len(),
        );
    }
    for accessor in &class.accessors {
        let name = rb_ident(&accessor.field);
        let _ = writeln!(
            out,
            "    class.define_method(\"{name}\", ::magnus::method!({}::{name}, 0))?;",
            class.name,
        );
        let _ = writeln!(
            out,
            "    class.define_method(\"{name}=\", ::magnus::method!({}::set_{name}, 1))?;",
            class.name,
        );
    }
    for method in &class.methods {
        let _ = writeln!(
            out,
            "    class.define_method(\"{}\", ::magnus::method!({}::{}, {}))?;",
            rb_ident(&method.name),
            class.name,
            method.name,
            method.params.len(),
        );
    }
    for associated in &class.statics {
        let _ = writeln!(
            out,
            "    class.define_singleton_method(\"{}\", ::magnus::function!({}::{}, {}))?;",
            rb_ident(&associated.name),
            class.name,
            associated.name,
            associated.params.len(),
        );
    }
    out
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

fn docs(doc: Option<&str>, indent: &str) -> String {
    match doc {
        Some(text) => format!("{indent}/// {text}\n"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ownership, Param, Receiver, Ty};
    use crate::plan::BindingPlan;

    /// `fn deviations_into(&self, values: &[f64], out: &mut [f64])`: the
    /// shape that surfaced the write-back requirement, an owned buffer
    /// alongside a writable one on the same call.
    fn deviations_into() -> Function {
        Function {
            symbol: "deviations_into".into(),
            rust_path: "acme::Summary::deviations_into".into(),
            name: "deviations_into".into(),
            receiver: Receiver::Shared,
            params: vec![
                Param {
                    name: "values".into(),
                    ty: Ty::List(Box::new(Ty::F64)),
                    ownership: Ownership::Borrowed,
                },
                Param {
                    name: "out".into(),
                    ty: Ty::List(Box::new(Ty::F64)),
                    ownership: Ownership::BorrowedMut,
                },
            ],
            ret: Ty::Unit,
            throws: None,
            is_async: false,
            doc: None,
        }
    }

    #[test]
    fn a_writable_buffer_parameter_writes_its_mutation_back_into_the_callers_array() {
        let rendered = member(&deviations_into(), &BindingPlan::default());
        assert!(rendered.contains("out: ::magnus::RArray"), "{rendered}");
        assert!(
            rendered.contains("let mut out_vec = out.to_vec::<f64>()?;"),
            "{rendered}"
        );
        assert!(rendered.contains("&mut out_vec"), "{rendered}");
        assert!(
            rendered.contains(
                "out_vec.iter().enumerate().try_for_each(|(i, value)| out.store(i as isize, *value))?;"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains("-> Result<(), ::magnus::Error>"),
            "a writable buffer's to_vec/store can fail, so the call becomes fallible: {rendered}"
        );
        assert!(
            rendered.contains("fn deviations_into(&self,"),
            "to_vec/store need no Ruby handle, so the receiver keeps plain \
             `&self` sugar rather than gaining an unused leading parameter: \
             {rendered}"
        );
    }
}
