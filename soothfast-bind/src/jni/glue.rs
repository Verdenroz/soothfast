//! The glue crate's `src/lib.rs`.
//!
//! No wrapper type is ever defined here: a handle crosses as the crate's
//! own type, boxed and cast through a bare `long`, because Java's own type
//! checker is what stops one class's pointer reaching another's method;
//! by the time a value is a `long` on the Rust side it is already trusted.
//! A borrowed buffer is pinned through `GetPrimitiveArrayCritical`; when a
//! call takes more than one, only the last one to matter (the writable one,
//! if there is one) stays pinned, since `AutoElementsCritical` holds a
//! mutable borrow of `env` and only one can be live at a time.

use std::fmt::Write;

use crate::model::{Ownership, Param, Primitive, Receiver, Ty};
use crate::naming;
use crate::plan::{Accessor, BindingPlan, Class, Function, Transfer};
use crate::{BindOptions, GENERATED_RS};

use super::types;

/// Local names the glue itself binds; a same-named parameter shadows a
/// escaped copy instead of colliding with them.
const RESERVED: &[&str] = &["env", "ptr", "class", "reason", "value"];

fn rust_ident(name: &str) -> String {
    naming::escape(name, RESERVED)
}

/// Every call that can fail throws through this rather than repeating the
/// same "unless a Java exception is already pending" check inline.
const PRELUDE: &str = "
fn __throw_unless_pending(env: &mut ::jni::JNIEnv, pending: bool, message: String) {
    if !pending {
        let _ = env.throw_new(\"java/lang/RuntimeException\", message);
    }
}
";

/// Whether anything in the surface can produce the kind of failure that
/// throws through [`PRELUDE`]'s helper: a text or buffer parameter, or a
/// string/sequence return, each read through `env`'s fallible calls.
fn needs_throw_helper(plan: &BindingPlan) -> bool {
    plan.functions().any(|f| {
        ret_needs_env(&f.ret)
            || f.params.iter().any(|p| {
                matches!(
                    Transfer::of(p, plan),
                    Transfer::Text { .. } | Transfer::Buffer { .. }
                )
            })
    }) || plan
        .classes
        .iter()
        .flat_map(|c| c.accessors.iter())
        .any(|a| ret_needs_env(&a.ty))
}

/// Render the glue crate body.
pub(crate) fn render(plan: &BindingPlan, opts: &BindOptions) -> String {
    let krate = format!("::{}", opts.crate_name);
    let mut out = String::from(GENERATED_RS);
    if needs_throw_helper(plan) {
        out.push_str(PRELUDE);
    }

    for class in &plan.classes {
        if class.is_plain_enum() {
            out.push_str(&enum_conversions(class, &krate, plan));
        }
    }
    for class in plan.classes.iter().filter(|c| !c.is_plain_enum()) {
        out.push_str(&class_block(class, &krate, plan, opts));
    }
    for function in &plan.functions {
        out.push_str(&shim(function, None, &krate, plan, opts));
    }
    out
}

/// Whether anything returns `name` by value, or reads it from a field: the
/// only two ways `{name}_to_ordinal` gets called.
fn needs_to_ordinal(name: &str, plan: &BindingPlan) -> bool {
    plan.functions()
        .any(|f| matches!(&f.ret, Ty::Class(n) if n == name))
        || plan
            .classes
            .iter()
            .flat_map(|c| c.accessors.iter())
            .any(|a| matches!(&a.ty, Ty::Class(n) if n == name))
}

/// Whether anything takes `name` by value as a parameter: the only way
/// `{name}_from_ordinal` gets called.
fn needs_from_ordinal(name: &str, plan: &BindingPlan) -> bool {
    plan.functions()
        .flat_map(|f| f.params.iter())
        .any(|p| matches!(&p.ty, Ty::Class(n) if n == name))
}

/// A payload-free enum crosses as its ordinal; both directions are decided
/// once here, in declaration order, matching the Java enum's own ordinals.
/// Only the directions the surface actually uses are emitted, so neither
/// conversion ever sits unused.
fn enum_conversions(class: &Class, krate: &str, plan: &BindingPlan) -> String {
    let inner = inner_path(&class.rust_path, krate);
    let name = types::snake(&class.name);
    let names: Vec<&str> = class
        .variants
        .iter()
        .flatten()
        .map(|v| v.name.as_str())
        .collect();
    let mut out = String::new();
    if needs_to_ordinal(&class.name, plan) {
        let mut to_arms = String::new();
        for (i, variant) in names.iter().enumerate() {
            let _ = writeln!(to_arms, "        {inner}::{variant} => {i},");
        }
        let _ = write!(
            out,
            "\nfn {name}_to_ordinal(value: &{inner}) -> i32 {{\n    match value {{\n{to_arms}    }}\n}}\n"
        );
    }
    if needs_from_ordinal(&class.name, plan) {
        let mut from_arms = String::new();
        for (i, variant) in names.iter().enumerate() {
            let _ = writeln!(from_arms, "        {i} => Ok({inner}::{variant}),");
        }
        let _ = write!(
            out,
            "\nfn {name}_from_ordinal(value: i32) -> Result<{inner}, i32> {{\n    match value {{\n{from_arms}        other => Err(other),\n    }}\n}}\n"
        );
    }
    out
}

fn class_block(class: &Class, krate: &str, plan: &BindingPlan, opts: &BindOptions) -> String {
    let mut out = String::new();
    if let Some(ctor) = &class.ctor {
        out.push_str(&shim(ctor, Some(class), krate, plan, opts));
    }
    for accessor in &class.accessors {
        out.push_str(&getter(accessor, class, krate, plan, opts));
    }
    for function in class.methods.iter().chain(class.statics.iter()) {
        out.push_str(&shim(function, Some(class), krate, plan, opts));
    }
    out.push_str(&free_shim(class, krate, opts));
    out
}

fn free_shim(class: &Class, krate: &str, opts: &BindOptions) -> String {
    let symbol = types::jni_symbol(&opts.package, &class.name, "nativeFree");
    let inner = inner_path(&class.rust_path, krate);
    format!(
        "\n/// Release a `{}` this library returned. Releasing one twice, or one\n\
         /// this library did not return, is undefined.\n\
         #[unsafe(no_mangle)]\n\
         pub extern \"system\" fn {symbol}<'local>(\n    _env: ::jni::JNIEnv<'local>,\n    \
         _class: ::jni::objects::JClass<'local>,\n    ptr: i64,\n) {{\n    \
         drop(unsafe {{ Box::from_raw(ptr as *mut {inner}) }});\n}}\n",
        class.name,
    )
}

/// A field read, which clones except for a mirrored enum (read by reference
/// instead, since it has no derived `Clone`): an exported handle type held
/// by a field never reaches here, because the plan reports it instead.
fn getter(
    accessor: &Accessor,
    class: &Class,
    krate: &str,
    plan: &BindingPlan,
    opts: &BindOptions,
) -> String {
    let native_name = types::native_method_name(&accessor.field);
    let symbol = types::jni_symbol(&opts.package, &class.name, &native_name);
    let inner = inner_path(&class.rust_path, krate);
    let native_ty = types::native_return_ty(&accessor.ty, plan);
    let ret = if native_ty.is_empty() {
        String::new()
    } else {
        format!(" -> {native_ty}")
    };
    let field = format!(
        "(unsafe {{ &*(ptr as *const {inner}) }}).{}",
        accessor.field
    );
    let needs_env = ret_needs_env(&accessor.ty);
    let env_param = env_param(needs_env, needs_env);
    // A mirrored plain enum has no derived `Clone`; reading it as `&self`
    // matches its reference straight into the ordinal instead of cloning an
    // owned copy just to consume it.
    let body = match &accessor.ty {
        Ty::Class(name) if plan.is_mirrored(name) => {
            format!("{}_to_ordinal(&{field})", types::snake(name))
        }
        _ => {
            let zero = zero_value(&accessor.ty, plan);
            format!(
                "let __out = {field}.clone();\n    {}",
                returned("__out", &accessor.ty, plan, &zero)
            )
        }
    };
    format!(
        "\n#[unsafe(no_mangle)]\npub extern \"system\" fn {symbol}<'local>(\n    {env_param},\n    _class: ::jni::objects::JClass<'local>,\n    ptr: i64,\n){ret} {{\n    {body}\n}}\n"
    )
}

/// Whether the body will need `env` at all. Every path that does can also
/// throw on failure (see `err_arm`), which needs `env` exclusively, so the
/// two questions have the same answer here.
fn env_usage(function: &Function, plan: &BindingPlan) -> (bool, bool) {
    let needs_env = function.throws.is_some()
        || ret_needs_env(&function.ret)
        || function.params.iter().any(|p| {
            matches!(
                Transfer::of(p, plan),
                Transfer::Text { .. }
                    | Transfer::Buffer { .. }
                    | Transfer::Handle { mirrored: true, .. }
            )
        });
    (needs_env, needs_env)
}

fn ret_needs_env(ty: &Ty) -> bool {
    matches!(ty, Ty::Str)
        || matches!(ty, Ty::Optional(inner) if **inner == Ty::Str)
        || types::element(ty).is_some()
}

fn env_param(needs_env: bool, needs_mut: bool) -> &'static str {
    match (needs_env, needs_mut) {
        (true, true) => "mut env: ::jni::JNIEnv<'local>",
        (true, false) => "env: ::jni::JNIEnv<'local>",
        (false, _) => "_env: ::jni::JNIEnv<'local>",
    }
}

/// One exported call: the `extern "system"` function JNI looks up.
fn shim(
    function: &Function,
    owner: Option<&Class>,
    krate: &str,
    plan: &BindingPlan,
    opts: &BindOptions,
) -> String {
    let has_receiver = owner.is_some_and(|_| function.receiver != Receiver::None);
    let native_name = types::native_method_name(&function.name);
    let holder = owner
        .map(|c| c.name.clone())
        .unwrap_or_else(|| types::module_class(&opts.package));
    let symbol = types::jni_symbol(&opts.package, &holder, &native_name);

    let (needs_env, needs_mut) = env_usage(function, plan);
    let mut params = vec![
        env_param(needs_env, needs_mut).to_string(),
        "_class: ::jni::objects::JClass<'local>".to_string(),
    ];
    if has_receiver {
        params.push("ptr: i64".to_string());
    }
    for param in &function.params {
        params.push(format!(
            "{}: {}",
            rust_ident(&param.name),
            types::native_param_ty(&param.ty, plan)
        ));
    }
    let native_ret = types::native_return_ty(&function.ret, plan);
    let ret_clause = if native_ret.is_empty() {
        String::new()
    } else {
        format!(" -> {native_ret}")
    };

    format!(
        "\n#[unsafe(no_mangle)]\npub extern \"system\" fn {symbol}<'local>(\n    {}\n){ret_clause} {{\n{}}}\n",
        params.join(",\n    "),
        body(function, owner, krate, plan, opts),
    )
}

/// The function body: convert every parameter, make the call, and shape
/// whatever it returns into the value the signature promised.
fn body(
    function: &Function,
    owner: Option<&Class>,
    krate: &str,
    plan: &BindingPlan,
    opts: &BindOptions,
) -> String {
    let pinned = pinned_param(function, plan);
    let zero = zero_value(&function.ret, plan);
    let mut out = String::new();

    if let Some(class) = owner
        && function.receiver != Receiver::None
    {
        let inner = inner_path(&class.rust_path, krate);
        let cast = match function.receiver {
            Receiver::Exclusive => format!("&mut *(ptr as *mut {inner})"),
            _ => format!("&*(ptr as *const {inner})"),
        };
        let _ = writeln!(out, "    let this = unsafe {{ {cast} }};");
    }
    for param in &function.params {
        if pinned.as_ref().is_some_and(|p| p.param.name == param.name) {
            continue;
        }
        out.push_str(&param_prelude(param, plan, krate, &zero));
    }
    if let Some(p) = &pinned {
        out.push_str(&pin_stmt(p, &zero));
    }

    let call = call_expr(function, owner, krate, plan, pinned.as_ref());
    let no_value = function.throws.is_none() && function.ret == Ty::Unit;
    if no_value {
        let _ = writeln!(out, "    {call};");
        if let Some(p) = &pinned {
            let _ = writeln!(out, "    drop({}_pin);", rust_ident(&p.param.name));
        }
        out.push_str(&writeback_stmts(function, plan, pinned.as_ref(), &zero));
        return out;
    }

    let _ = writeln!(out, "    let __out = {call};");
    if let Some(p) = &pinned {
        let _ = writeln!(out, "    drop({}_pin);", rust_ident(&p.param.name));
    }
    out.push_str(&writeback_stmts(function, plan, pinned.as_ref(), &zero));
    let _ = writeln!(out, "{}", finish(function, plan, opts));
    out
}

/// Every non-pinned writable buffer's write-back, in parameter order. The
/// pinned buffer, if any, already writes back through `CopyBack` when its
/// guard drops above, so it is excluded here.
fn writeback_stmts(
    function: &Function,
    plan: &BindingPlan,
    pinned: Option<&Pinned>,
    zero: &str,
) -> String {
    let mut out = String::new();
    for param in &function.params {
        if pinned.is_some_and(|p| p.param.name == param.name) {
            continue;
        }
        if let Transfer::Buffer {
            element,
            writable: true,
            ..
        } = Transfer::of(param, plan)
        {
            out.push_str(&writeback_stmt(&rust_ident(&param.name), element, zero));
        }
    }
    out
}

/// The single buffer parameter staying pinned across a call, carrying the
/// element type and writability [`pin_stmt`] and [`call_arg`] need so
/// neither has to re-derive them from `Transfer::of` and handle the
/// buffer-or-not case a second time.
struct Pinned<'a> {
    param: &'a Param,
    element: Primitive,
    writable: bool,
}

/// Which single buffer parameter, if more than one crosses, gets to stay
/// pinned across the call: `AutoElementsCritical` holds a mutable borrow of
/// `env`, so at most one guard can be alive at a time. The writable one
/// wins, since only it saves two copies rather than one; a lone borrowed
/// buffer wins by default.
fn pinned_param<'a>(function: &'a Function, plan: &BindingPlan) -> Option<Pinned<'a>> {
    let buffers: Vec<(&Param, Primitive, bool)> = function
        .params
        .iter()
        .filter_map(|p| match Transfer::of(p, plan) {
            Transfer::Buffer {
                element,
                borrowed: true,
                writable,
            } => Some((p, element, writable)),
            _ => None,
        })
        .collect();
    buffers
        .iter()
        .find(|(_, _, writable)| *writable)
        .copied()
        .or(if buffers.len() == 1 {
            buffers.first().copied()
        } else {
            None
        })
        .map(|(param, element, writable)| Pinned {
            param,
            element,
            writable,
        })
}

/// The statement converting one non-pinned parameter into the shape the
/// Rust call needs, or nothing for a parameter that already arrives in it.
/// `zero` is the enclosing call's own zero, for the early return a failed
/// JNI call takes instead of panicking under `panic = "abort"`.
fn param_prelude(param: &Param, plan: &BindingPlan, krate: &str, zero: &str) -> String {
    let name = rust_ident(&param.name);
    match Transfer::of(param, plan) {
        Transfer::Text { nullable: true, .. } => format!(
            "    let {name}: Option<String> = if {name}.is_null() {{\n        None\n    }} else {{\n        match env.get_string(&{name}) {{\n            Ok(v) => Some(v.into()),\n            {}\n        }}\n    }};\n",
            err_arm(zero),
        ),
        Transfer::Text { .. } => format!(
            "    let {name}: String = match env.get_string(&{name}) {{\n        Ok(v) => v.into(),\n        {}\n    }};\n",
            err_arm(zero),
        ),
        Transfer::Buffer {
            element,
            borrowed: false,
            ..
        } => owned_buffer_stmt(&name, element, zero),
        Transfer::Buffer {
            element,
            writable: true,
            ..
        } => readback_buffer_stmt(&name, element, zero),
        Transfer::Buffer { element, .. } => copied_buffer_stmt(&name, element, zero),
        Transfer::Handle { mirrored: true, .. } => {
            let snake = types::snake(&class_name(&param.ty));
            let class = class_name(&param.ty);
            format!(
                "    let {name} = match {snake}_from_ordinal({name}) {{\n        Ok(v) => v,\n        Err(other) => {{\n            let _ = env.throw_new(\"java/lang/IllegalArgumentException\", format!(\"invalid {class} ordinal: {{other}}\"));\n            return {zero};\n        }}\n    }};\n"
            )
        }
        Transfer::Handle { writable, .. } => {
            let class = class_name(&param.ty);
            let inner = class_rust_ty(&class, plan, krate);
            let cast = match writable {
                true => format!("&mut *({name} as *mut {inner})"),
                false => format!("&*({name} as *const {inner})"),
            };
            format!("    let {name} = unsafe {{ {cast} }};\n")
        }
        _ => scalar_cast_stmt(&name, &param.ty),
    }
}

/// An owned buffer (`Vec<T>` taken by value): a plain array-region read,
/// since nothing about it needs to stay pinned for the caller to see.
fn owned_buffer_stmt(name: &str, element: Primitive, zero: &str) -> String {
    let spelling = types::scalar_of(element);
    let natural = element.render();
    let region = region_rust(element);
    let cast = element_cast("__raw", natural, region);
    let err = err_arm(zero);
    format!(
        "    let __len = match env.get_array_length(&{name}) {{\n        Ok(v) => v as usize,\n        {err}\n    }};\n\
         \x20   let mut __raw = vec![{zero_elem}; __len];\n\
         \x20   match env.get_{java}_array_region(&{name}, 0, &mut __raw) {{\n        Ok(()) => {{}}\n        {err}\n    }}\n\
         \x20   let {name}: Vec<{natural}> = {cast};\n",
        zero_elem = zero_literal(region),
        java = spelling.java,
    )
}

/// A writable buffer that will not stay pinned, because another one in the
/// same call needs the one live `AutoElementsCritical` guard instead: read
/// with a plain array-region call now, and write back with
/// [`writeback_stmt`] once the call and the pinned guard's drop are both
/// past, since a plain JNI call also needs `env` unborrowed.
fn readback_buffer_stmt(name: &str, element: Primitive, zero: &str) -> String {
    let spelling = types::scalar_of(element);
    let natural = element.render();
    let cast = element_cast("__raw", natural, spelling.rust);
    let err = err_arm(zero);
    format!(
        "    let __len = match env.get_array_length(&{name}) {{\n        Ok(v) => v as usize,\n        {err}\n    }};\n\
         \x20   let mut __raw = vec![{zero_elem}; __len];\n\
         \x20   match env.get_{java}_array_region(&{name}, 0, &mut __raw) {{\n        Ok(()) => {{}}\n        {err}\n    }}\n\
         \x20   let mut {name}_buf: Vec<{natural}> = {cast};\n",
        zero_elem = zero_literal(spelling.rust),
        java = spelling.java,
    )
}

/// [`readback_buffer_stmt`]'s other half: write `{name}_buf` back into the
/// Java array it was read from.
fn writeback_stmt(name: &str, element: Primitive, zero: &str) -> String {
    let spelling = types::scalar_of(element);
    let natural = element.render();
    let cast = element_cast(&format!("{name}_buf"), spelling.rust, natural);
    format!(
        "    let {name}_out: Vec<{stored}> = {cast};\n\
         \x20   match env.set_{java}_array_region(&{name}, 0, &{name}_out) {{\n        Ok(()) => {{}}\n        {err}\n    }}\n",
        stored = spelling.rust,
        java = spelling.java,
        err = err_arm(zero),
    )
}

/// A borrowed buffer that will not stay pinned, because another one in the
/// same call needs the one live `AutoElementsCritical` guard instead.
fn copied_buffer_stmt(name: &str, element: Primitive, zero: &str) -> String {
    let natural = element.render();
    let region = region_rust(element);
    let cast = if natural == region {
        "guard.to_vec()".to_string()
    } else if natural == "bool" {
        "guard.iter().map(|v| *v != 0).collect()".to_string()
    } else {
        format!("guard.iter().map(|v| *v as {natural}).collect()")
    };
    format!(
        "    let {name}: Vec<{natural}> = {{\n\
         \x20       let result = unsafe {{ env.get_array_elements_critical(&{name}, ::jni::objects::ReleaseMode::NoCopyBack) }};\n\
         {err_block}\n\
         \x20       let guard = result.unwrap_or_else(|_| unreachable!(\"checked above\"));\n\
         \x20       {cast}\n\
         \x20   }};\n",
        err_block = critical_err_block(zero, "        "),
    )
}

fn pin_stmt(pinned: &Pinned, zero: &str) -> String {
    let name = rust_ident(&pinned.param.name);
    let mode = if pinned.writable {
        "CopyBack"
    } else {
        "NoCopyBack"
    };
    let binding = if pinned.writable { "let mut" } else { "let" };
    format!(
        "    let result = unsafe {{ env.get_array_elements_critical(&{name}, ::jni::objects::ReleaseMode::{mode}) }};\n\
         {err_block}\n\
         \x20   {binding} {name}_pin = result.unwrap_or_else(|_| unreachable!(\"checked above\"));\n",
        err_block = critical_err_block(zero, "    "),
    )
}

/// The `Err` arm for a critical-array acquisition specifically. Every other
/// fallible call here can throw straight out of its own `Err` arm, but
/// `AutoElementsCritical`'s `Ok` value borrows `env` itself, so the borrow
/// checker won't let `env` be reborrowed to throw until the `Result`
/// holding that borrow is dropped first. `indent` matches the caller's own
/// block so the leading comment lines up with the `if` below it.
fn critical_err_block(zero: &str, indent: &str) -> String {
    format!(
        "{indent}// env stays borrowed until result is dropped, so this can't throw as a plain match arm.\n\
         {indent}if let Err(ref e) = result {{\n\
         {indent}    let pending = matches!(e, ::jni::errors::Error::JavaException);\n\
         {indent}    let message = e.to_string();\n\
         {indent}    drop(result);\n\
         {indent}    __throw_unless_pending(&mut env, pending, message);\n\
         {indent}    return {zero};\n\
         {indent}}}"
    )
}

/// The pinned slice or mutable slice a call argument passes, reinterpreting
/// the pinned element type when the Rust side wants its unsigned twin (or,
/// for `bool`, its `jboolean` byte: JNI guarantees that one is always 0 or
/// 1, which is what makes reinterpreting it as `bool` sound).
fn pinned_slice_expr(name: &str, element: Primitive, writable: bool) -> String {
    let natural = element.render();
    let stored = region_rust(element);
    let var = format!("{name}_pin");
    if natural == stored {
        return match writable {
            true => format!("&mut {var}"),
            false => format!("&{var}"),
        };
    }
    match writable {
        true => format!(
            "unsafe {{ ::std::slice::from_raw_parts_mut({var}.as_mut_ptr() as *mut {natural}, {var}.len()) }}"
        ),
        false => format!(
            "unsafe {{ ::std::slice::from_raw_parts({var}.as_ptr() as *const {natural}, {var}.len()) }}"
        ),
    }
}

fn scalar_cast_stmt(name: &str, ty: &Ty) -> String {
    let natural = ty.render();
    let Some(spelling) = types::scalar(ty) else {
        return String::new();
    };
    match natural == spelling.rust {
        true => String::new(),
        false => format!("    let {name} = {name} as {natural};\n"),
    }
}

/// The Rust type `env.get_/set_{java}_array_region` and a critical-array
/// guard hand over: the same as the element's natural type for every
/// primitive but `bool`, whose JNI array calls read and write a `jboolean`
/// byte rather than a native `bool`.
fn region_rust(element: Primitive) -> &'static str {
    match element {
        Primitive::Bool => "u8",
        other => types::scalar_of(other).rust,
    }
}

fn element_cast(expr: &str, natural: &str, stored: &str) -> String {
    if natural == stored {
        return expr.to_string();
    }
    if natural == "bool" {
        return format!("{expr}.into_iter().map(|v| v != 0).collect()");
    }
    format!("{expr}.into_iter().map(|v| v as {natural}).collect()")
}

fn zero_literal(rust_ty: &str) -> &'static str {
    match rust_ty {
        "f32" | "f64" => "0.0",
        "bool" => "false",
        _ => "0",
    }
}

/// The `Err` arm every fallible JNI call shares: throw unless a Java
/// exception is already pending (the JNI contract says to leave that one
/// alone), then hand back the caller's zero. `panic = "abort"` in the glue
/// crate's manifest is exactly why this can never be a bare panic: there is
/// no `catch_unwind` between here and the JVM.
fn err_arm(zero: &str) -> String {
    format!(
        "Err(e) => {{\n            let pending = matches!(e, ::jni::errors::Error::JavaException);\n            __throw_unless_pending(&mut env, pending, e.to_string());\n            return {zero};\n        }}"
    )
}

/// The Rust call, with every parameter already converted or pinned.
fn call_expr(
    function: &Function,
    owner: Option<&Class>,
    krate: &str,
    plan: &BindingPlan,
    pinned: Option<&Pinned>,
) -> String {
    let args: Vec<String> = function
        .params
        .iter()
        .map(|p| call_arg(p, plan, pinned))
        .collect();
    let args = args.join(", ");
    match (owner, function.receiver) {
        (Some(_), Receiver::None) => format!(
            "{}::{}({args})",
            inner_path(owner_path(&function.rust_path), krate),
            function.name,
        ),
        (Some(_), _) => format!("this.{}({args})", function.name),
        (None, _) => format!("{}({args})", inner_path(&function.rust_path, krate)),
    }
}

fn call_arg(param: &Param, plan: &BindingPlan, pinned: Option<&Pinned>) -> String {
    let name = rust_ident(&param.name);
    if let Some(p) = pinned
        && p.param.name == param.name
    {
        return pinned_slice_expr(&name, p.element, p.writable);
    }
    if matches!(
        Transfer::of(param, plan),
        Transfer::Buffer { writable: true, .. }
    ) {
        return format!("&mut {name}_buf");
    }
    // The prelude already converted a mirrored enum into an owned value of
    // the real type; a non-mirrored handle's prelude already cast into a
    // reference, so only the mirrored case still needs one taken here.
    if let Transfer::Handle { mirrored: true, .. } = Transfer::of(param, plan) {
        return match param.ownership {
            Ownership::Owned => name,
            Ownership::Borrowed => format!("&{name}"),
            Ownership::BorrowedMut => format!("&mut {name}"),
        };
    }
    if matches!(Transfer::of(param, plan), Transfer::Handle { .. }) {
        return name;
    }
    if let Transfer::Text {
        nullable: true,
        borrowed,
    } = Transfer::of(param, plan)
    {
        return match borrowed {
            true => format!("{name}.as_deref()"),
            false => name,
        };
    }
    match param.ownership {
        Ownership::BorrowedMut => format!("&mut {name}"),
        Ownership::Borrowed => format!("&{name}"),
        Ownership::Owned => name,
    }
}

/// The call's result, converted into whatever the signature promised. A
/// failing call throws the package exception and hands back a zero the
/// caller cannot read: the pending exception fires before Java evaluates it.
fn finish(function: &Function, plan: &BindingPlan, opts: &BindOptions) -> String {
    let zero = zero_value(&function.ret, plan);
    match &function.throws {
        None => format!("    {}", returned("__out", &function.ret, plan, &zero)),
        Some(_) => {
            let ok = returned("value", &function.ret, plan, &zero);
            let exception = exception_jni_path(opts);
            format!(
                "    match __out {{\n        Ok(value) => {{\n            {ok}\n        }}\n        Err(reason) => {{\n            let _ = env.throw_new(\"{exception}\", ::std::string::ToString::to_string(&reason));\n            {zero}\n        }}\n    }}"
            )
        }
    }
}

/// A value leaving the user's crate, shaped into the native return type.
/// `zero` is the value returned in place of one a failed JNI call cannot
/// produce; see `err_arm`.
fn returned(expr: &str, ty: &Ty, plan: &BindingPlan, zero: &str) -> String {
    match ty {
        Ty::Unit => expr.to_string(),
        Ty::Str => format!(
            "match env.new_string({expr}) {{\n        Ok(v) => v.into_raw(),\n        {}\n    }}",
            err_arm(zero),
        ),
        Ty::Class(name) if plan.is_mirrored(name) => {
            format!("{}_to_ordinal(&{expr})", types::snake(name))
        }
        Ty::Class(_) => format!("Box::into_raw(Box::new({expr})) as i64"),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(_) => format!(
                "match {expr} {{ Some(v) => Box::into_raw(Box::new(v)) as i64, None => 0i64 }}"
            ),
            Ty::Str => format!(
                "match {expr} {{\n        Some(v) => match env.new_string(v) {{\n            Ok(s) => s.into_raw(),\n            {}\n        }},\n        None => ::std::ptr::null_mut(),\n    }}",
                err_arm(zero),
            ),
            other => returned(expr, other, plan, zero),
        },
        _ => match types::element(ty) {
            Some((natural, spelling)) => array_return_expr(expr, &natural, &spelling, zero),
            None => scalar_return_expr(expr, ty),
        },
    }
}

fn scalar_return_expr(expr: &str, ty: &Ty) -> String {
    let natural = ty.render();
    let Some(spelling) = types::scalar(ty) else {
        return expr.to_string();
    };
    match natural == spelling.rust {
        true => expr.to_string(),
        false => format!("{expr} as {}", spelling.rust),
    }
}

fn array_return_expr(expr: &str, natural: &str, spelling: &types::Spelling, zero: &str) -> String {
    let region = match natural {
        "bool" => "u8",
        _ => spelling.rust,
    };
    let cast = element_cast(expr, region, natural);
    let err = err_arm(zero);
    format!(
        "{{\n            let __values: Vec<{rust}> = {cast};\n            \
         let __arr = match env.new_{java}_array(__values.len() as i32) {{\n                Ok(v) => v,\n                {err}\n            }};\n            \
         match env.set_{java}_array_region(&__arr, 0, &__values) {{\n                Ok(()) => {{}}\n                {err}\n            }}\n            \
         __arr.into_raw()\n        }}",
        rust = region,
        java = spelling.java,
    )
}

fn zero_value(ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Unit => String::new(),
        Ty::Bool => "false".into(),
        Ty::F32 | Ty::F64 => "0.0".into(),
        Ty::Str => "::std::ptr::null_mut()".into(),
        Ty::Optional(inner) if **inner == Ty::Str => "::std::ptr::null_mut()".into(),
        Ty::Class(name) if plan.is_mirrored(name) => "0".into(),
        Ty::Class(_) | Ty::Optional(_) => "0i64".into(),
        ty if types::element(ty).is_some() => "::std::ptr::null_mut()".into(),
        _ => "0".into(),
    }
}

/// The slash-separated internal name `throw_new` looks a class up by.
fn exception_jni_path(opts: &BindOptions) -> String {
    format!(
        "{}/{}Exception",
        opts.package.replace('.', "/"),
        types::module_class(&opts.package)
    )
}

fn class_name(ty: &Ty) -> String {
    match ty {
        Ty::Class(name) => name.clone(),
        Ty::Optional(inner) => class_name(inner),
        _ => String::new(),
    }
}

/// The fully qualified Rust type behind an exported class name.
fn class_rust_ty(name: &str, plan: &BindingPlan, krate: &str) -> String {
    plan.classes
        .iter()
        .find(|c| c.name == name)
        .map(|c| inner_path(&c.rust_path, krate))
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
