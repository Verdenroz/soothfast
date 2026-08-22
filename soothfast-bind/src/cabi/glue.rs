//! The glue crate's `src/lib.rs`.
//!
//! Every exported item becomes one `extern "C"` function under the flat
//! symbol the plan already assigns. There is no marshaling framework here:
//! a handle is a pointer the caller owns until it frees it, a sequence is a
//! pointer and a length, and a failure is a message written through an
//! out-parameter.

use std::collections::BTreeMap;

use crate::model::{Receiver, Ty};
use crate::plan::{Accessor, BindingPlan, Class, Function, Transfer};
use crate::{BindOptions, GENERATED_RS, GLUE_ALLOW};

use super::types::{self, array_rust, handle_rust};

/// Render the glue crate body.
pub(crate) fn render(plan: &BindingPlan, opts: &BindOptions) -> String {
    let krate = format!("::{}", opts.crate_name);
    let module = &opts.module;
    let mut out = String::from(GENERATED_RS);
    out.push_str(GLUE_ALLOW);
    out.push_str(PRELUDE);

    for (ty, name) in arrays(plan) {
        out.push_str(&array_type(&ty, &name, module));
    }
    if returns_text(plan) {
        out.push_str(&string_free(module));
    }
    for class in &plan.classes {
        out.push_str(&class_block(class, &krate, plan, opts));
    }
    for function in &plan.functions {
        out.push_str(&free_fn(function, &krate, plan, opts));
    }
    out
}

/// Every sequence element the surface hands back, one owned struct each.
pub(crate) fn arrays(plan: &BindingPlan) -> Vec<(Ty, String)> {
    let returns = plan.functions().map(|f| &f.ret);
    let fields = plan
        .classes
        .iter()
        .flat_map(|c| c.accessors.iter())
        .map(|a| &a.ty);
    let mut wanted: BTreeMap<String, Ty> = BTreeMap::new();
    for ty in returns.chain(fields) {
        if types::element(ty).is_some() {
            wanted.insert(ty.render(), ty.clone());
        }
    }
    wanted
        .into_values()
        .map(|ty| {
            let element = types::element(&ty).expect("checked").rust;
            (ty, element)
        })
        .collect()
}

/// Whether anything comes back as an owned string, which the caller has to
/// be given a way to release.
pub(crate) fn returns_text(plan: &BindingPlan) -> bool {
    plan.functions()
        .any(|f| f.ret == Ty::Str || f.throws.is_some())
        || plan
            .classes
            .iter()
            .flat_map(|c| c.accessors.iter())
            .any(|a| a.ty == Ty::Str)
}

/// One owned sequence struct, plus the call that releases it.
///
/// The buffer is boxed rather than kept as a `Vec`: `into_boxed_slice` is
/// what makes the capacity equal the length, so the pointer and the length
/// the caller holds are enough to reconstruct it.
fn array_type(ty: &Ty, element: &str, module: &str) -> String {
    let name = array_rust(ty, module);
    let free = format!("{}_free", types::array_c(ty, module));
    format!(
        "
/// An owned `{element}` sequence. Release it with `{free}`.
#[repr(C)]
pub struct {name} {{
    data: *mut {element},
    len: usize,
}}

impl {name} {{
    fn empty() -> Self {{
        {name} {{ data: ::std::ptr::null_mut(), len: 0 }}
    }}

    fn new(values: Vec<{element}>) -> Self {{
        let mut boxed = values.into_boxed_slice();
        let out = {name} {{ data: boxed.as_mut_ptr(), len: boxed.len() }};
        ::std::mem::forget(boxed);
        out
    }}
}}

/// Release a sequence this library returned. Releasing one twice, or one it
/// did not return, is undefined.
#[unsafe(no_mangle)]
pub unsafe extern \"C\" fn {free}(array: {name}) {{
    if array.data.is_null() {{
        return;
    }}
    drop(unsafe {{
        Box::from_raw(::std::ptr::slice_from_raw_parts_mut(array.data, array.len))
    }});
}}
"
    )
}

fn string_free(module: &str) -> String {
    format!(
        "
/// Release a string this library returned. Releasing one twice, or one it
/// did not return, is undefined.
#[unsafe(no_mangle)]
pub unsafe extern \"C\" fn {module}_string_free(text: *mut ::std::os::raw::c_char) {{
    if text.is_null() {{
        return;
    }}
    drop(unsafe {{ ::std::ffi::CString::from_raw(text) }});
}}
"
    )
}

fn class_block(class: &Class, krate: &str, plan: &BindingPlan, opts: &BindOptions) -> String {
    if class.is_plain_enum() {
        return plain_enum(class, krate, opts);
    }
    let name = handle_rust(&class.name, &opts.module);
    let mut out = format!(
        "
/// Opaque handle to `{}`.
pub struct {name}({});

/// Release a `{}`. Releasing one twice, or one this library did not return,
/// is undefined.
#[unsafe(no_mangle)]
pub unsafe extern \"C\" fn {}_free(handle: *mut {name}) {{
    if handle.is_null() {{
        return;
    }}
    drop(unsafe {{ Box::from_raw(handle) }});
}}
",
        class.name,
        inner_path(&class.rust_path, krate),
        class.name,
        types::handle_c(&class.name, &opts.module),
    );

    if let Some(ctor) = &class.ctor {
        out.push_str(&shim(ctor, Some(class), krate, plan, opts));
    }
    for accessor in &class.accessors {
        out.push_str(&getter(accessor, class, plan, opts));
    }
    for function in class.methods.iter().chain(class.statics.iter()) {
        out.push_str(&shim(function, Some(class), krate, plan, opts));
    }
    out
}

/// A payload-free enum mirrors onto a C enumeration rather than staying an
/// opaque handle, with conversions both ways so it crosses in either
/// direction.
fn plain_enum(class: &Class, krate: &str, opts: &BindOptions) -> String {
    let inner = inner_path(&class.rust_path, krate);
    let name = handle_rust(&class.name, &opts.module);
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
    let variants: String = names
        .iter()
        .enumerate()
        .map(|(i, n)| match i {
            0 => format!("    #[default]\n    {n},\n"),
            _ => format!("    {n},\n"),
        })
        .collect();
    format!(
        "
/// `{}`, mirrored as a C enumeration.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum {name} {{
{variants}}}

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
        class.name,
        arms(&inner, &name),
        arms(&name, &inner),
    )
}

/// A field read, which clones: an exported type held by a field never
/// reaches here, because the plan reports it instead.
fn getter(accessor: &Accessor, class: &Class, plan: &BindingPlan, opts: &BindOptions) -> String {
    let module = &opts.module;
    let handle = handle_rust(&class.name, module);
    let symbol = format!(
        "{}_{}",
        types::handle_c(&class.name, module),
        types::snake(&accessor.field)
    );
    let read = format!("(unsafe {{ &*handle }}).0.{}.clone()", accessor.field);
    format!(
        "
/// Read `{}` off a `{}`.
#[unsafe(no_mangle)]
pub unsafe extern \"C\" fn {symbol}(handle: *const {handle}) -> {} {{
    {}
}}
",
        accessor.field,
        class.name,
        returned_rust(&accessor.ty, plan, module),
        returned(&read, &accessor.ty, plan, module),
    )
}

/// One exported call, as the flat `extern "C"` function C sees.
fn shim(
    function: &Function,
    owner: Option<&Class>,
    krate: &str,
    plan: &BindingPlan,
    opts: &BindOptions,
) -> String {
    let module = &opts.module;
    let call = match (owner, function.receiver) {
        (Some(_), Receiver::None) => format!(
            "{}::{}({})",
            inner_path(owner_path(&function.rust_path), krate),
            function.name,
            args(function, plan)
        ),
        (Some(_), _) => format!(
            "{}.{}({})",
            receiver_expr(function.receiver),
            function.name,
            args(function, plan)
        ),
        (None, _) => format!(
            "{}({})",
            inner_path(&function.rust_path, krate),
            args(function, plan)
        ),
    };
    format!(
        "
{}#[unsafe(no_mangle)]
pub unsafe extern \"C\" fn {}({}){} {{
{}}}
",
        docs(function.doc.as_deref()),
        function.symbol,
        signature(function, owner, plan, opts),
        returns(function, plan, module),
        body(&call, function, plan, module),
    )
}

fn receiver_expr(receiver: Receiver) -> &'static str {
    match receiver {
        Receiver::Exclusive => "(unsafe { &mut *handle }).0",
        _ => "(unsafe { &*handle }).0",
    }
}

/// The generated signature's arguments: the receiver, then one or two per
/// parameter, then the error out-parameter a failing call writes through.
fn signature(
    function: &Function,
    owner: Option<&Class>,
    plan: &BindingPlan,
    opts: &BindOptions,
) -> String {
    let module = &opts.module;
    let mut out: Vec<String> = Vec::new();
    if let (Some(class), receiver) = (owner, function.receiver)
        && receiver != Receiver::None
    {
        let handle = handle_rust(&class.name, module);
        out.push(match receiver {
            Receiver::Exclusive => format!("handle: *mut {handle}"),
            _ => format!("handle: *const {handle}"),
        });
    }
    for param in &function.params {
        out.extend(param_decl(param, plan, module));
    }
    if function.throws.is_some() {
        out.push("error: *mut *mut ::std::os::raw::c_char".to_string());
    }
    out.join(", ")
}

/// One parameter, as the one or two C arguments it takes.
fn param_decl(param: &crate::model::Param, plan: &BindingPlan, module: &str) -> Vec<String> {
    let name = super::c_ident(&param.name);
    match Transfer::of(param, plan) {
        Transfer::Buffer {
            ref element,
            writable,
            ..
        } => {
            let rust = types::scalar(element).map(|s| s.rust).unwrap_or_default();
            let mutability = match writable {
                true => "mut",
                false => "const",
            };
            vec![
                format!("{name}: *{mutability} {rust}"),
                format!("{name}_len: usize"),
            ]
        }
        Transfer::Text { .. } => vec![format!("{name}: *const ::std::os::raw::c_char")],
        Transfer::Handle { mirrored: true, .. } => {
            vec![format!("{name}: {}", class_rust(&param.ty, module))]
        }
        Transfer::Handle { writable, .. } => {
            let handle = class_rust(&param.ty, module);
            let mutability = match writable {
                true => "mut",
                false => "const",
            };
            vec![format!("{name}: *{mutability} {handle}")]
        }
        _ => vec![format!(
            "{name}: {}",
            types::scalar(&param.ty).map(|s| s.rust).unwrap_or_default()
        )],
    }
}

/// The same parameters, as the expressions the Rust call takes.
fn args(function: &Function, plan: &BindingPlan) -> String {
    function
        .params
        .iter()
        .map(|p| {
            let name = super::c_ident(&p.name);
            match Transfer::of(p, plan) {
                Transfer::Buffer {
                    borrowed, writable, ..
                } => {
                    let slice = match writable {
                        true => format!("unsafe {{ ffi::slice_mut({name}, {name}_len) }}"),
                        false => format!("unsafe {{ ffi::slice({name}, {name}_len) }}"),
                    };
                    match borrowed {
                        true => slice,
                        false => format!("{slice}.to_vec()"),
                    }
                }
                Transfer::Text { borrowed } => {
                    let text = format!("unsafe {{ ffi::text({name}) }}");
                    match borrowed {
                        true => text,
                        false => format!("{text}.to_string()"),
                    }
                }
                Transfer::Handle { mirrored: true, .. } => format!("{name}.into()"),
                Transfer::Handle { writable: true, .. } => {
                    format!("&mut (unsafe {{ &mut *{name} }}).0")
                }
                Transfer::Handle { .. } => format!("&(unsafe {{ &*{name} }}).0"),
                _ => name,
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn returns(function: &Function, plan: &BindingPlan, module: &str) -> String {
    match &function.ret {
        Ty::Unit => String::new(),
        ty => format!(" -> {}", returned_rust(ty, plan, module)),
    }
}

/// A returned type, as the glue crate spells it.
fn returned_rust(ty: &Ty, plan: &BindingPlan, module: &str) -> String {
    match ty {
        Ty::Unit => String::new(),
        Ty::Str => "*mut ::std::os::raw::c_char".into(),
        Ty::Class(name) if plan.is_mirrored(name) => handle_rust(name, module),
        Ty::Class(name) => format!("*mut {}", handle_rust(name, module)),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) => format!("*mut {}", handle_rust(name, module)),
            _ => String::new(),
        },
        ty if types::element(ty).is_some() => array_rust(ty, module),
        ty => types::scalar(ty).map(|s| s.rust).unwrap_or_default(),
    }
}

/// A value leaving the user's crate, boxed or copied into the shape the
/// signature promised.
fn returned(expr: &str, ty: &Ty, plan: &BindingPlan, module: &str) -> String {
    match ty {
        Ty::Unit => expr.to_string(),
        Ty::Str => format!("ffi::into_text({expr})"),
        Ty::Class(name) if plan.is_mirrored(name) => format!("{expr}.into()"),
        Ty::Class(name) => format!(
            "Box::into_raw(Box::new({}({expr})))",
            handle_rust(name, module)
        ),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) => format!(
                "match {expr} {{ Some(v) => Box::into_raw(Box::new({}(v))), None => ::std::ptr::null_mut() }}",
                handle_rust(name, module)
            ),
            _ => expr.to_string(),
        },
        ty if types::element(ty).is_some() => {
            format!("{}::new({expr})", array_rust(ty, module))
        }
        _ => expr.to_string(),
    }
}

/// The call, converted into whatever the signature promised. A failing one
/// writes its message through `error` and hands back a zero the caller is
/// told not to read.
fn body(call: &str, function: &Function, plan: &BindingPlan, module: &str) -> String {
    let Some(_) = &function.throws else {
        return format!("    {}\n", returned(call, &function.ret, plan, module));
    };
    let ok = returned("value", &function.ret, plan, module);
    let zero = types::zero(&function.ret, plan, module);
    format!(
        "    match {call} {{
        Ok(value) => {{
            unsafe {{ ffi::clear(error) }};
            {ok}
        }}
        Err(reason) => {{
            unsafe {{ ffi::report(error, &reason) }};
            {zero}
        }}
    }}
"
    )
}

/// The glue crate's name for an exported type, reached directly or through
/// an `Option`.
fn class_rust(ty: &Ty, module: &str) -> String {
    match ty {
        Ty::Class(name) => handle_rust(name, module),
        Ty::Optional(inner) => class_rust(inner, module),
        _ => String::new(),
    }
}

fn free_fn(function: &Function, krate: &str, plan: &BindingPlan, opts: &BindOptions) -> String {
    shim(function, None, krate, plan, opts)
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

fn docs(doc: Option<&str>) -> String {
    match doc {
        Some(text) => format!("/// {text}\n"),
        None => String::new(),
    }
}

/// Helpers every generated crate needs, written once.
///
/// A C caller spells an empty sequence as a null pointer and a zero length,
/// which `from_raw_parts` will not accept, so the length decides first.
pub(crate) const PRELUDE: &str = r#"
/// Helpers the shims call. A module rather than bare functions: a parameter
/// named `text` would otherwise shadow the one that reads it.
mod ffi {
pub unsafe fn slice<'a, T>(data: *const T, len: usize) -> &'a [T] {
    match len {
        0 => &[],
        _ => unsafe { ::std::slice::from_raw_parts(data, len) },
    }
}

pub unsafe fn slice_mut<'a, T>(data: *mut T, len: usize) -> &'a mut [T] {
    match len {
        0 => &mut [],
        _ => unsafe { ::std::slice::from_raw_parts_mut(data, len) },
    }
}

pub unsafe fn text<'a>(data: *const ::std::os::raw::c_char) -> &'a str {
    if data.is_null() {
        return "";
    }
    unsafe { ::std::ffi::CStr::from_ptr(data) }.to_str().unwrap_or("")
}

pub fn into_text(value: String) -> *mut ::std::os::raw::c_char {
    match ::std::ffi::CString::new(value) {
        Ok(text) => text.into_raw(),
        Err(_) => ::std::ptr::null_mut(),
    }
}

pub unsafe fn clear(error: *mut *mut ::std::os::raw::c_char) {
    if !error.is_null() {
        unsafe { *error = ::std::ptr::null_mut() };
    }
}

pub unsafe fn report<E: ::std::fmt::Display>(error: *mut *mut ::std::os::raw::c_char, reason: &E) {
    if !error.is_null() {
        unsafe { *error = into_text(reason.to_string()) };
    }
}
}
"#;
