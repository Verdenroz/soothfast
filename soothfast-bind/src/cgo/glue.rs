//! `<name>.go`: the cgo wrapper over the C backend's own ABI.
//!
//! Every exported item becomes idiomatic Go over the flat symbols the C
//! backend already declared: a handle is a struct with an unexported
//! pointer, closed through its `*_free` and backstopped by a finalizer, and
//! a failing call becomes a Go `error`.

use std::fmt::Write;

use crate::GENERATED_GO;
use crate::cabi::glue::{arrays, returns_text};
use crate::cabi::types as c;
use crate::model::{Param, Receiver, Ty};
use crate::plan::{Accessor, BindingPlan, Class, Function, Transfer};

use super::types::{self as go, ident};

/// Render the package's one Go source file.
///
/// `package` names the Go package, the embedded C library, and the header
/// `#include`s: `c_opts` makes all three the same sanitized name before this
/// runs, so there is only one name to thread through here.
pub(crate) fn render(plan: &BindingPlan, package: &str) -> String {
    let mut out = format!("{GENERATED_GO}\npackage {package}\n\n");
    out.push_str(&cgo_preamble(package));
    out.push_str(&imports(plan));

    if returns_text(plan) {
        out.push_str(&string_helper(package));
    }
    if needs_buf_ptr(plan) {
        out.push_str(BUF_PTR);
    }
    for (ty, element) in arrays(plan) {
        out.push_str(&array_helper(&ty, &element, package));
    }
    for class in &plan.classes {
        out.push_str(&class_block(class, plan, package));
    }
    for function in &plan.functions {
        out.push_str(&func_block(function, None, false, plan));
    }
    out
}

fn cgo_preamble(module: &str) -> String {
    format!(
        "/*\n#cgo CFLAGS: -I${{SRCDIR}}\n#cgo LDFLAGS: -L${{SRCDIR}}/target/release -l{module} \
         -Wl,-rpath,${{SRCDIR}}/target/release\n#include \"{module}.h\"\n#include <stdlib.h>\n*/\n\
         import \"C\"\n\n"
    )
}

fn needs_errors(plan: &BindingPlan) -> bool {
    plan.functions().any(|f| f.throws.is_some())
}

fn needs_runtime(plan: &BindingPlan) -> bool {
    plan.classes.iter().any(|c| !c.is_plain_enum())
}

fn needs_buf_ptr(plan: &BindingPlan) -> bool {
    plan.functions()
        .any(|f| f.params.iter().any(|p| is_buffer(p, plan)))
}

fn needs_unsafe(plan: &BindingPlan) -> bool {
    needs_buf_ptr(plan)
        || plan
            .functions()
            .any(|f| matches!(&f.ret, Ty::List(_) | Ty::Bytes))
        || plan
            .classes
            .iter()
            .flat_map(|c| c.accessors.iter())
            .any(|a| matches!(&a.ty, Ty::List(_) | Ty::Bytes))
}

fn is_buffer(param: &Param, plan: &BindingPlan) -> bool {
    matches!(Transfer::of(param, plan), Transfer::Buffer { .. })
}

fn imports(plan: &BindingPlan) -> String {
    let mut names = Vec::new();
    if needs_errors(plan) {
        names.push("errors");
    }
    if needs_runtime(plan) {
        names.push("runtime");
    }
    if needs_unsafe(plan) {
        names.push("unsafe");
    }
    if names.is_empty() {
        return String::new();
    }
    let body: String = names.iter().map(|n| format!("\t\"{n}\"\n")).collect();
    format!("import (\n{body})\n\n")
}

const BUF_PTR: &str = "\
// bufPtr returns a pointer to a slice's backing array, or nil for an empty
// one: taking the address of an empty slice's element panics.
func bufPtr[T any](s []T) *T {
\tif len(s) == 0 {
\t\treturn nil
\t}
\treturn &s[0]
}

";

fn string_helper(module: &str) -> String {
    format!(
        "// goString copies a string this library returned and releases it.\n\
         func goString(s *C.char) string {{\n\
         \tdefer C.{module}_string_free(s)\n\
         \treturn C.GoString(s)\n\
         }}\n\n"
    )
}

fn array_helper(ty: &Ty, element: &str, module: &str) -> String {
    let go_ty = go::element_go(ty);
    let c_arr = c::array_c(ty, module);
    let free = format!("{c_arr}_free");
    let fn_name = format!("{go_ty}Slice");
    format!(
        "// {fn_name} copies a `{element}` sequence this library returned and\n\
         // releases it.\n\
         func {fn_name}(arr C.{c_arr}) []{go_ty} {{\n\
         \tdefer C.{free}(arr)\n\
         \tif arr.len == 0 {{\n\
         \t\treturn nil\n\
         \t}}\n\
         \tout := make([]{go_ty}, arr.len)\n\
         \tcopy(out, unsafe.Slice((*{go_ty})(unsafe.Pointer(arr.data)), arr.len))\n\
         \treturn out\n\
         }}\n\n"
    )
}

fn class_block(class: &Class, plan: &BindingPlan, module: &str) -> String {
    if class.is_plain_enum() {
        return plain_enum(class, module);
    }
    let name = &class.name;
    let handle = c::handle_c(name, module);
    let mut out = format!(
        "{}// {name} wraps a native {handle}.\ntype {name} struct {{\n\tptr *C.{handle}\n}}\n\n\
         func wrap{name}(ptr *C.{handle}) *{name} {{\n\
         \th := &{name}{{ptr: ptr}}\n\
         \truntime.SetFinalizer(h, (*{name}).Close)\n\
         \treturn h\n\
         }}\n\n\
         // Close releases the native handle. Calling it twice, or on a\n\
         // {name} that was never open, is a no-op.\n\
         func (recv *{name}) Close() error {{\n\
         \tif recv.ptr == nil {{\n\
         \t\treturn nil\n\
         \t}}\n\
         \tC.{handle}_free(recv.ptr)\n\
         \trecv.ptr = nil\n\
         \truntime.SetFinalizer(recv, nil)\n\
         \treturn nil\n\
         }}\n\n",
        docs(class.doc.as_deref()),
    );

    if let Some(ctor) = &class.ctor {
        out.push_str(&func_block(ctor, Some(class), true, plan));
    }
    for accessor in &class.accessors {
        out.push_str(&getter(accessor, class, plan, module));
    }
    for function in class.methods.iter().chain(class.statics.iter()) {
        out.push_str(&func_block(function, Some(class), false, plan));
    }
    out
}

fn plain_enum(class: &Class, module: &str) -> String {
    let name = &class.name;
    let c_ty = c::handle_c(name, module);
    let mut out = format!(
        "{}type {name} int32\n\nconst (\n",
        docs(class.doc.as_deref()),
    );
    let variants: Vec<&str> = class
        .variants
        .iter()
        .flatten()
        .map(|v| v.name.as_str())
        .collect();
    let width = variants
        .iter()
        .map(|v| name.len() + v.len())
        .max()
        .unwrap_or(0);
    for (i, variant) in variants.iter().enumerate() {
        let ident = format!("{name}{variant}");
        // gofmt aligns consecutive const declarations on the type column.
        let _ = writeln!(out, "\t{ident:width$} {name} = {i}");
    }
    let _ = writeln!(out, ")\n");
    let _ = write!(
        out,
        "// c converts a {name} to the native {c_ty} it mirrors.\n\
         func (recv {name}) c() C.{c_ty} {{\n\treturn C.{c_ty}(recv)\n}}\n\n"
    );
    out
}

fn getter(accessor: &Accessor, class: &Class, plan: &BindingPlan, module: &str) -> String {
    let go_name = c::pascal(&accessor.field);
    let symbol = format!(
        "{}_{}",
        c::handle_c(&class.name, module),
        c::snake(&accessor.field)
    );
    let ret_ty = go_return_type(&accessor.ty, plan);
    let call = format!("C.{symbol}(recv.ptr)");
    format!(
        "{}func (recv *{}) {go_name}() {ret_ty} {{\n\treturn {}\n}}\n\n",
        docs(accessor.doc.as_deref()),
        class.name,
        convert_ret(&call, &accessor.ty, plan),
    )
}

fn func_block(
    function: &Function,
    owner: Option<&Class>,
    is_ctor: bool,
    plan: &BindingPlan,
) -> String {
    let go_name = match (owner, is_ctor) {
        (Some(c), true) => format!("New{}", c.name),
        (Some(c), false) if function.receiver == Receiver::None => {
            format!("{}{}", c.name, c::pascal(&function.name))
        }
        (Some(_), false) => c::pascal(&function.name),
        (None, _) => c::pascal(&function.name),
    };
    let recv_sig = match (owner, is_ctor, function.receiver) {
        (Some(c), false, r) if r != Receiver::None => format!("(recv *{}) ", c.name),
        _ => String::new(),
    };
    let params = go_signature_params(&function.params, plan);
    let ret_sig = go_return_signature(function, plan);
    let call = c_call(function, owner, is_ctor, function.receiver, plan);

    format!(
        "{}func {recv_sig}{go_name}({params}){ret_sig} {{\n{}}}\n\n",
        docs(function.doc.as_deref()),
        body(function, &call, plan),
    )
}

fn go_signature_params(params: &[Param], plan: &BindingPlan) -> String {
    params
        .iter()
        .map(|p| format!("{} {}", ident(&p.name), go_param_type(p, plan)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn go_param_type(param: &Param, plan: &BindingPlan) -> String {
    match Transfer::of(param, plan) {
        Transfer::Buffer { .. } => format!("[]{}", go::element_go(&param.ty)),
        Transfer::Text { .. } => "string".into(),
        Transfer::Handle { mirrored: true, .. } => class_name(&param.ty).to_string(),
        Transfer::Handle { .. } => format!("*{}", class_name(&param.ty)),
        _ => go_return_type(&param.ty, plan),
    }
}

fn go_return_type(ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Unit => String::new(),
        Ty::Str => "string".into(),
        Ty::Class(name) if plan.is_mirrored(name) => name.clone(),
        Ty::Class(name) => format!("*{name}"),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) => format!("*{name}"),
            _ => String::new(),
        },
        Ty::Bytes => "[]byte".into(),
        Ty::List(inner) => format!("[]{}", go::scalar(inner).map_or("byte", |s| s.go)),
        ty => go::scalar(ty).map_or(String::new(), |s| s.go.to_string()),
    }
}

fn go_return_signature(f: &Function, plan: &BindingPlan) -> String {
    let base = go_return_type(&f.ret, plan);
    match (&f.throws, f.ret == Ty::Unit) {
        (Some(_), true) => " error".into(),
        (Some(_), false) => format!(" ({base}, error)"),
        (None, true) => String::new(),
        (None, false) => format!(" {base}"),
    }
}

fn class_name(ty: &Ty) -> &str {
    match ty {
        Ty::Class(name) => name,
        Ty::Optional(inner) => class_name(inner),
        _ => "",
    }
}

/// The receiver, then one C argument per parameter, then the error
/// out-parameter a failing call writes through.
fn c_call(
    function: &Function,
    owner: Option<&Class>,
    is_ctor: bool,
    receiver: Receiver,
    plan: &BindingPlan,
) -> String {
    let mut args = Vec::new();
    if !is_ctor && owner.is_some() && receiver != Receiver::None {
        args.push("recv.ptr".to_string());
    }
    for param in &function.params {
        args.push(c_arg(param, plan));
    }
    if function.throws.is_some() {
        args.push("&errPtr".to_string());
    }
    format!("C.{}({})", function.symbol, args.join(", "))
}

fn c_arg(param: &Param, plan: &BindingPlan) -> String {
    let name = ident(&param.name);
    match Transfer::of(param, plan) {
        Transfer::Buffer { element, .. } => {
            let c_elem = c::scalar(&element).map_or(String::new(), |s| s.c);
            format!("(*C.{c_elem})(unsafe.Pointer(bufPtr({name}))), C.size_t(len({name}))")
        }
        Transfer::Text { .. } => c_string_var(&param.name),
        Transfer::Handle { mirrored: true, .. } => format!("{name}.c()"),
        Transfer::Handle { .. } => format!("{name}.ptr"),
        _ => match go::scalar(&param.ty) {
            Some(s) => format!("{}({name})", s.cgo),
            None => name,
        },
    }
}

fn c_string_var(param_name: &str) -> String {
    format!("c{}", c::pascal(param_name))
}

/// `CString` conversions every text parameter needs before the call, freed
/// once the call returns.
fn text_prep(function: &Function, plan: &BindingPlan) -> String {
    let mut out = String::new();
    for param in &function.params {
        if let Transfer::Text { .. } = Transfer::of(param, plan) {
            let cvar = c_string_var(&param.name);
            let _ = writeln!(out, "\t{cvar} := C.CString({})", ident(&param.name));
            let _ = writeln!(out, "\tdefer C.free(unsafe.Pointer({cvar}))");
        }
    }
    out
}

fn convert_ret(expr: &str, ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Unit => expr.to_string(),
        Ty::Str => format!("goString({expr})"),
        Ty::Class(name) if plan.is_mirrored(name) => format!("{name}({expr})"),
        Ty::Class(name) => format!("wrap{name}({expr})"),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) if plan.is_mirrored(name) => format!(
                "func() *{name} {{ if p := {expr}; p != nil {{ v := {name}(*p); return &v }}; return nil }}()"
            ),
            Ty::Class(name) => format!(
                "func() *{name} {{ if p := {expr}; p != nil {{ return wrap{name}(p) }}; return nil }}()"
            ),
            _ => expr.to_string(),
        },
        ty if c::element(ty).is_some() => format!("{}Slice({expr})", go::element_go(ty)),
        _ => match go::scalar(ty) {
            Some(s) => format!("{}({expr})", s.go),
            None => expr.to_string(),
        },
    }
}

fn go_zero(ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Unit => String::new(),
        Ty::Bool => "false".into(),
        Ty::Str => "\"\"".into(),
        Ty::F32 | Ty::F64 => "0".into(),
        Ty::Class(name) if plan.is_mirrored(name) => format!("{name}(0)"),
        Ty::Class(_) | Ty::Optional(_) | Ty::List(_) | Ty::Bytes => "nil".into(),
        _ => "0".into(),
    }
}

/// The call, converted into whatever the signature promised. A failing call
/// reads its message off `errPtr` and hands back a zero value.
fn body(function: &Function, call: &str, plan: &BindingPlan) -> String {
    let mut out = text_prep(function, plan);
    if function.throws.is_none() {
        let _ = match function.ret {
            Ty::Unit => writeln!(out, "\t{call}"),
            _ => writeln!(out, "\treturn {}", convert_ret(call, &function.ret, plan)),
        };
        return out;
    }
    let _ = writeln!(out, "\tvar errPtr *C.char");
    let _ = writeln!(out, "\tret := {call}");
    let _ = writeln!(out, "\tif errPtr != nil {{");
    let _ = writeln!(out, "\t\terr := errors.New(goString(errPtr))");
    let fail = match function.ret {
        Ty::Unit => "err".to_string(),
        _ => format!("{}, err", go_zero(&function.ret, plan)),
    };
    let _ = writeln!(out, "\t\treturn {fail}");
    let _ = writeln!(out, "\t}}");
    let ok = match function.ret {
        Ty::Unit => "nil".to_string(),
        _ => format!("{}, nil", convert_ret("ret", &function.ret, plan)),
    };
    let _ = writeln!(out, "\treturn {ok}");
    out
}

fn docs(doc: Option<&str>) -> String {
    match doc {
        Some(text) => format!("// {text}\n"),
        None => String::new(),
    }
}
