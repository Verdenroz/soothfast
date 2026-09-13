//! `<name>.hpp`: the header-only RAII wrapper over the C backend's own ABI.
//!
//! A handle owns its pointer through a `unique_ptr` with a stateless
//! deleter calling the C `*_free`; a failing call reads the `char **error`
//! out-parameter and throws; a plain enum crosses as an `enum class` cast
//! to and from the C enum it mirrors.

use std::fmt::Write;

use crate::model::{Param, Receiver, Ty};
use crate::plan::{Accessor, BindingPlan, Class, Function, Transfer};
use crate::{BindOptions, GENERATED_CPP};

use super::types::ident;
use crate::cabi::glue::{arrays, returns_text};
use crate::cabi::types as c;

const INCLUDES: &str = "\
#include <cstdint>
#include <memory>
#include <optional>
#include <span>
#include <stdexcept>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

";

const ERROR_CLASS: &str = "\
class Error : public std::runtime_error {
public:
    explicit Error(const std::string &message) : std::runtime_error(message) {}
};

";

/// Render the header. `module` is the flattened C module name
/// (`super::c_opts(opts).module`), which is what the embedded C header and
/// its symbols are actually named after; `opts.package` is the untouched
/// `::`-delimited namespace path.
pub(crate) fn render(plan: &BindingPlan, opts: &BindOptions, module: &str) -> String {
    let mut out = String::from(GENERATED_CPP);
    let _ = write!(out, "\n#pragma once\n\n#include \"{module}.h\"\n\n");
    out.push_str(INCLUDES);
    let _ = writeln!(out, "namespace {} {{\n", opts.package);
    out.push_str(ERROR_CLASS);

    if returns_text(plan) {
        out.push_str(&string_helper(module));
    }
    for (ty, _) in arrays(plan) {
        out.push_str(&array_helper(&ty, module));
    }
    for class in plan.classes.iter().filter(|c| c.is_plain_enum()) {
        out.push_str(&plain_enum(class));
    }
    for class in plan.classes.iter().filter(|c| !c.is_plain_enum()) {
        out.push_str(&handle_class(class, plan, module));
    }
    for function in &plan.functions {
        out.push_str(&shim(function, None, plan, module, false));
    }

    let _ = writeln!(out, "}} // namespace {}", opts.package);
    out
}

fn string_helper(module: &str) -> String {
    format!(
        "inline std::string take_string(char *raw) {{\n\
         \x20   std::string value(raw);\n\
         \x20   {module}_string_free(raw);\n\
         \x20   return value;\n\
         }}\n\n"
    )
}

/// One `to_vector_<element>` helper, copying a returned sequence into a
/// `std::vector` and freeing the C one, the same shape as `goString`/
/// `float64Slice` on the Go side.
fn array_helper(ty: &Ty, module: &str) -> String {
    let c_arr = c::array_c(ty, module);
    let free = format!("{c_arr}_free");
    let scalar = c::element(ty).map(|s| s.c).unwrap_or_default();
    let name = array_helper_name(ty);
    format!(
        "inline std::vector<{scalar}> {name}({c_arr} arr) {{\n\
         \x20   std::vector<{scalar}> out(arr.data, arr.data + arr.len);\n\
         \x20   {free}(arr);\n\
         \x20   return out;\n\
         }}\n\n",
    )
}

fn array_helper_name(ty: &Ty) -> String {
    let element = c::element(ty).map(|s| s.rust).unwrap_or_default();
    format!("to_vector_{element}")
}

fn plain_enum(class: &Class) -> String {
    let variants: String = class
        .variants
        .iter()
        .flatten()
        .map(|v| format!("    {},\n", v.name))
        .collect();
    format!(
        "{}enum class {} {{\n{variants}}};\n\n",
        docs(class.doc.as_deref(), ""),
        class.name,
    )
}

fn handle_class(class: &Class, plan: &BindingPlan, module: &str) -> String {
    let c_ty = c::handle_c(&class.name, module);
    let mut out = format!(
        "{}class {} {{\npublic:\n",
        docs(class.doc.as_deref(), ""),
        class.name
    );

    if let Some(ctor) = &class.ctor {
        out.push_str(&constructor(ctor, class, plan, module));
    }
    let _ = writeln!(
        out,
        "    // Adopts a pointer another bound call already returned; C++ has \
         no package-private tier to hide this behind.\n    \
         explicit {}({c_ty} *raw) : handle_(raw) {{}}\n",
        class.name
    );
    let _ = writeln!(
        out,
        "    {c_ty} *get() const noexcept {{ return handle_.get(); }}\n"
    );
    for accessor in &class.accessors {
        out.push_str(&getter(accessor, class, plan, module));
    }
    for method in &class.methods {
        out.push_str(&shim(method, Some(class), plan, module, false));
    }
    for stat in &class.statics {
        out.push_str(&shim(stat, Some(class), plan, module, true));
    }

    out.push_str("private:\n");
    let _ = writeln!(
        out,
        "    struct Deleter {{\n        void operator()({c_ty} *p) const noexcept {{ {c_ty}_free(p); }}\n    }};\n"
    );
    let _ = writeln!(out, "    std::unique_ptr<{c_ty}, Deleter> handle_;");
    out.push_str("};\n\n");
    out
}

/// The plan's constructor, as a real C++ constructor. A throwing one
/// delegates to a private `construct` factory in its member-initializer
/// list, since a constructor cannot check an out-parameter and bail before
/// `handle_` is built any other way.
fn constructor(ctor: &Function, class: &Class, plan: &BindingPlan, module: &str) -> String {
    let params = signature_params(&ctor.params, plan);
    let doc = docs(ctor.doc.as_deref(), "    ");
    match &ctor.throws {
        None => {
            let call = format!(
                "{}({})",
                ctor.symbol,
                c_args(ctor, Some(class), plan, module)
            );
            format!(
                "{doc}    explicit {}({params}) : handle_({call}) {{}}\n\n",
                class.name
            )
        }
        Some(_) => {
            let names: Vec<String> = ctor.params.iter().map(|p| ident(&p.name)).collect();
            let mut out = format!(
                "{doc}    explicit {}({params}) : handle_(construct({})) {{}}\n\n",
                class.name,
                names.join(", "),
            );
            out.push_str(&construct_helper(ctor, class, plan, module));
            out
        }
    }
}

/// The raw call behind a throwing constructor, returning the built pointer
/// rather than a wrapped instance: `handle_` still has to be constructed
/// from it in the real constructor's initializer list.
fn construct_helper(ctor: &Function, class: &Class, plan: &BindingPlan, module: &str) -> String {
    let params = signature_params(&ctor.params, plan);
    let call = format!(
        "{}({})",
        ctor.symbol,
        c_args(ctor, Some(class), plan, module)
    );
    let c_ty = c::handle_c(&class.name, module);
    let mut out = format!("    static {c_ty} *construct({params}) {{\n");
    out.push_str(&text_prep(ctor, plan, "        "));
    let _ = writeln!(out, "        char *error = nullptr;");
    let _ = writeln!(out, "        auto *raw = {call};");
    out.push_str(&throw_check(module, "        "));
    let _ = writeln!(out, "        return raw;");
    out.push_str("    }\n\n");
    out
}

fn getter(accessor: &Accessor, class: &Class, plan: &BindingPlan, module: &str) -> String {
    let ret_ty = cpp_return_type(&accessor.ty);
    let symbol = format!(
        "{}_{}",
        c::handle_c(&class.name, module),
        c::snake(&accessor.field)
    );
    let call = format!("{symbol}(handle_.get())");
    format!(
        "{doc}    {ret_ty} {name}() const {{\n        auto raw_result = {call};\n        return {conv};\n    }}\n\n",
        doc = docs(accessor.doc.as_deref(), "    "),
        name = ident(&accessor.field),
        conv = returned("raw_result", &accessor.ty, plan),
    )
}

/// One exported call: a method, a static, or a free function.
fn shim(
    function: &Function,
    owner: Option<&Class>,
    plan: &BindingPlan,
    module: &str,
    is_static: bool,
) -> String {
    let ret_ty = cpp_return_type(&function.ret);
    let params = signature_params(&function.params, plan);
    let indent = if owner.is_some() { "    " } else { "" };
    let prefix = match (owner, is_static) {
        (Some(_), true) => "static ",
        (Some(_), false) => "",
        (None, _) => "inline ",
    };
    let qualifier = match (owner, function.receiver) {
        (Some(_), Receiver::Shared) => " const",
        _ => "",
    };
    let call = format!(
        "{}({})",
        function.symbol,
        c_args(function, owner, plan, module)
    );
    format!(
        "{doc}{indent}{prefix}{ret_ty} {name}({params}){qualifier} {{\n{body}{indent}}}\n\n",
        doc = docs(function.doc.as_deref(), indent),
        name = ident(&function.name),
        body = body(function, &call, plan, module, indent),
    )
}

/// The call, converted into whatever the signature promised. A throwing
/// call reads its message off the local `error` it declares, frees it, and
/// throws before returning anything.
fn body(function: &Function, call: &str, plan: &BindingPlan, module: &str, indent: &str) -> String {
    let inner = format!("{indent}    ");
    let mut out = text_prep(function, plan, &inner);
    let unit = function.ret == Ty::Unit;
    if function.throws.is_some() {
        let _ = writeln!(out, "{inner}char *error = nullptr;");
    }
    match unit {
        true => {
            let _ = writeln!(out, "{inner}{call};");
        }
        false => {
            let _ = writeln!(out, "{inner}auto raw_result = {call};");
        }
    }
    if function.throws.is_some() {
        out.push_str(&throw_check(module, &inner));
    }
    if !unit {
        let _ = writeln!(
            out,
            "{inner}return {};",
            returned("raw_result", &function.ret, plan)
        );
    }
    out
}

fn throw_check(module: &str, indent: &str) -> String {
    format!(
        "{indent}if (error != nullptr) {{\n\
         {indent}    std::string message(error);\n\
         {indent}    {module}_string_free(error);\n\
         {indent}    throw Error(message);\n\
         {indent}}}\n"
    )
}

/// `std::string` locals for every text parameter, built before the call so
/// its `.c_str()` is guaranteed null-terminated: a `string_view` need not
/// be.
fn text_prep(function: &Function, plan: &BindingPlan, indent: &str) -> String {
    let mut out = String::new();
    for param in &function.params {
        if let Transfer::Text { .. } = Transfer::of(param, plan) {
            let name = ident(&param.name);
            let _ = writeln!(out, "{indent}std::string {name}_owned({name});");
        }
    }
    out
}

fn signature_params(params: &[Param], plan: &BindingPlan) -> String {
    params
        .iter()
        .map(|p| format!("{} {}", cpp_param_type(p, plan), ident(&p.name)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn cpp_param_type(param: &Param, plan: &BindingPlan) -> String {
    match Transfer::of(param, plan) {
        Transfer::Buffer {
            element, writable, ..
        } => {
            let scalar = c::scalar(&element).map(|s| s.c).unwrap_or_default();
            match writable {
                true => format!("std::span<{scalar}>"),
                false => format!("std::span<const {scalar}>"),
            }
        }
        Transfer::Text { .. } => "std::string_view".into(),
        Transfer::Handle { mirrored: true, .. } => class_name(&param.ty).to_string(),
        Transfer::Handle { writable, .. } => {
            let name = class_name(&param.ty);
            match writable {
                true => format!("{name}&"),
                false => format!("const {name}&"),
            }
        }
        _ => c::scalar(&param.ty).map(|s| s.c).unwrap_or_default(),
    }
}

/// A returned type, as the wrapper declares it.
fn cpp_return_type(ty: &Ty) -> String {
    match ty {
        Ty::Unit => "void".into(),
        Ty::Str => "std::string".into(),
        Ty::Class(name) => name.clone(),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) => format!("std::optional<{name}>"),
            _ => "void".into(),
        },
        ty if c::element(ty).is_some() => {
            let scalar = c::element(ty).map(|s| s.c).unwrap_or_default();
            format!("std::vector<{scalar}>")
        }
        _ => c::scalar(ty).map(|s| s.c).unwrap_or_default(),
    }
}

/// A value the C call handed back, converted into what the signature
/// promised. `expr` is always a plain local's name, never a call: nothing
/// here evaluates it twice.
fn returned(expr: &str, ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Unit => expr.to_string(),
        Ty::Str => format!("take_string({expr})"),
        Ty::Class(name) if plan.is_mirrored(name) => format!("static_cast<{name}>({expr})"),
        Ty::Class(name) => format!("{name}({expr})"),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) => {
                format!("{expr} != nullptr ? std::optional<{name}>({name}({expr})) : std::nullopt")
            }
            _ => expr.to_string(),
        },
        ty if c::element(ty).is_some() => format!("{}({expr})", array_helper_name(ty)),
        _ => expr.to_string(),
    }
}

/// The C call's arguments: the receiver, then one or two per parameter,
/// then the `error` out-parameter a throwing call declares.
fn c_args(function: &Function, owner: Option<&Class>, plan: &BindingPlan, module: &str) -> String {
    let mut out = Vec::new();
    if let (Some(_), receiver) = (owner, function.receiver)
        && receiver != Receiver::None
    {
        out.push("handle_.get()".to_string());
    }
    for param in &function.params {
        out.extend(c_arg(param, plan, module));
    }
    if function.throws.is_some() {
        out.push("&error".to_string());
    }
    out.join(", ")
}

fn c_arg(param: &Param, plan: &BindingPlan, module: &str) -> Vec<String> {
    let name = ident(&param.name);
    match Transfer::of(param, plan) {
        Transfer::Buffer { .. } => vec![format!("{name}.data()"), format!("{name}.size()")],
        Transfer::Text { .. } => vec![format!("{name}_owned.c_str()")],
        Transfer::Handle { mirrored: true, .. } => {
            let c_enum = c::handle_c(class_name(&param.ty), module);
            vec![format!("static_cast<{c_enum}>({name})")]
        }
        Transfer::Handle { .. } => vec![format!("{name}.get()")],
        _ => vec![name],
    }
}

/// The C++ name of an exported type, reached directly or through an
/// `Option`.
fn class_name(ty: &Ty) -> &str {
    match ty {
        Ty::Class(name) => name,
        Ty::Optional(inner) => class_name(inner),
        _ => "",
    }
}

fn docs(doc: Option<&str>, indent: &str) -> String {
    match doc {
        Some(text) => format!("{indent}// {text}\n"),
        None => String::new(),
    }
}
