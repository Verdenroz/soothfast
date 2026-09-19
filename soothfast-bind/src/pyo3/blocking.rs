//! The `{name}_blocking` twin of every async call, emitted under
//! `[[bind]] blocking = true`: the same signature without `async`, running
//! the future to completion on the package's own runtime so a plain script
//! or a notebook never has to `asyncio.run` a call. It releases the
//! interpreter lock while it waits whenever everything the call touches can
//! leave the Python thread.

use std::fmt::Write;

use crate::BindOptions;
use crate::model::Receiver;
use crate::plan::{BindingPlan, Class, Function, detachable};

use super::glue::{call_args, docs, inner_path, join, owner_path, params, return_ty, shaped_body};
use super::py_ident;

pub(crate) fn name(function: &Function) -> String {
    format!("{}_blocking", py_ident(&function.name))
}

fn wanted(function: &Function, opts: &BindOptions) -> bool {
    opts.blocking && function.is_async
}

/// The twin as a method, or `None` when the call is not async or twins
/// are not configured.
pub(crate) fn member(
    method: &Function,
    plan: &BindingPlan,
    owner: &Class,
    opts: &BindOptions,
) -> Option<String> {
    if !wanted(method, opts) {
        return None;
    }
    let detach = detachable(method, Some(owner), plan);
    let receiver = match method.receiver {
        Receiver::Exclusive => "&mut self",
        _ => "&self",
    };
    let call = format!("self.0.{}({})", method.name, call_args(method, plan));
    let args = join(receiver, &params(method, plan, detach));
    Some(format!(
        "{}    fn {}({args}) -> {} {{\n{}    }}\n",
        doc(method),
        name(method),
        return_ty(method, plan),
        body(&call, method, plan, "        ", detach),
    ))
}

/// The twin as a static method.
pub(crate) fn associated(
    function: &Function,
    krate: &str,
    plan: &BindingPlan,
    owner: &Class,
    opts: &BindOptions,
) -> Option<String> {
    if !wanted(function, opts) {
        return None;
    }
    let detach = detachable(function, Some(owner), plan);
    let call = format!(
        "{}::{}({})",
        inner_path(owner_path(&function.rust_path), krate),
        function.name,
        call_args(function, plan)
    );
    Some(format!(
        "{}    #[staticmethod]\n    fn {}({}) -> {} {{\n{}    }}\n",
        doc(function),
        name(function),
        params(function, plan, detach),
        return_ty(function, plan),
        body(&call, function, plan, "        ", detach),
    ))
}

/// The twin as a free function.
pub(crate) fn free(
    function: &Function,
    krate: &str,
    plan: &BindingPlan,
    opts: &BindOptions,
) -> Option<String> {
    if !wanted(function, opts) {
        return None;
    }
    let detach = detachable(function, None, plan);
    let call = format!(
        "{}({})",
        inner_path(&function.rust_path, krate),
        call_args(function, plan)
    );
    Some(format!(
        "\n{}#[pyfunction]\nfn {}({}) -> {} {{\n{}}}\n",
        doc(function),
        name(function),
        params(function, plan, detach),
        return_ty(function, plan),
        body(&call, function, plan, "    ", detach),
    ))
}

/// One `m.add_function` per free twin.
pub(crate) fn registrations(plan: &BindingPlan, opts: &BindOptions) -> String {
    let mut out = String::new();
    for function in plan.functions.iter().filter(|f| wanted(f, opts)) {
        let _ = writeln!(
            out,
            "    m.add_function(wrap_pyfunction!({}, m)?)?;",
            name(function)
        );
    }
    out
}

/// A twin's name already taken by a member of the same class, or by another
/// function of the module, is an error rather than a silent override.
pub(crate) fn check(plan: &BindingPlan, opts: &BindOptions) -> Result<(), String> {
    for class in &plan.classes {
        let members: Vec<String> = class
            .methods
            .iter()
            .chain(&class.statics)
            .map(|f| py_ident(&f.name))
            .chain(class.accessors.iter().map(|a| py_ident(&a.field)))
            .collect();
        for function in class.methods.iter().chain(&class.statics) {
            if wanted(function, opts) && members.contains(&name(function)) {
                return Err(format!(
                    "{}: the blocking twin of `{}` would be named `{}`, which the class already has",
                    opts.package,
                    function.name,
                    name(function)
                ));
            }
        }
    }
    let functions: Vec<String> = plan.functions.iter().map(|f| py_ident(&f.name)).collect();
    for function in plan.functions.iter().filter(|f| wanted(f, opts)) {
        if functions.contains(&name(function)) {
            return Err(format!(
                "{}: the blocking twin of `{}` would be named `{}`, which the module already has",
                opts.package,
                function.name,
                name(function)
            ));
        }
    }
    Ok(())
}

fn body(call: &str, function: &Function, plan: &BindingPlan, indent: &str, detach: bool) -> String {
    shaped_body(
        &format!("runtime().block_on({call})"),
        function,
        plan,
        indent,
        detach,
    )
}

fn doc(function: &Function) -> String {
    docs(
        Some(&format!(
            "Blocking form of `{}`: runs the call to completion on the package's runtime.",
            py_ident(&function.name)
        )),
        "    ",
    )
}
