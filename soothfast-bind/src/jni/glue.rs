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

use crate::model::{Ownership, Param, Receiver, Ty};
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
#[allow(dead_code)]
fn __throw_unless_pending(env: &mut ::jni::JNIEnv, pending: bool, message: String) {
    if !pending {
        let _ = env.throw_new(\"java/lang/RuntimeException\", message);
    }
}
";

/// Render the glue crate body.
pub(crate) fn render(plan: &BindingPlan, opts: &BindOptions) -> String {
    let krate = format!("::{}", opts.crate_name);
    let mut out = String::from(GENERATED_RS);
    out.push_str(PRELUDE);

    for class in &plan.classes {
        if class.is_plain_enum() {
            out.push_str(&enum_conversions(class, &krate));
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

/// A payload-free enum crosses as its ordinal; both directions are decided
/// once here, in declaration order, matching the Java enum's own ordinals.
fn enum_conversions(class: &Class, krate: &str) -> String {
    let inner = inner_path(&class.rust_path, krate);
    let name = types::snake(&class.name);
    let names: Vec<&str> = class
        .variants
        .iter()
        .flatten()
        .map(|v| v.name.as_str())
        .collect();
    let mut to_arms = String::new();
    let mut from_arms = String::new();
    for (i, variant) in names.iter().enumerate() {
        let _ = writeln!(to_arms, "        {inner}::{variant} => {i},");
        let _ = writeln!(from_arms, "        {i} => {inner}::{variant},");
    }
    format!(
        "\n#[allow(dead_code)]\nfn {name}_to_ordinal(value: {inner}) -> i32 {{\n    match value {{\n{to_arms}    }}\n}}\n\n#[allow(dead_code)]\nfn {name}_from_ordinal(value: i32) -> {inner} {{\n    match value {{\n{from_arms}        _ => unreachable!(\"Java only ever sends back one of this enum's own ordinals\"),\n    }}\n}}\n"
    )
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

/// A field read, which clones: an exported type held by a field never
/// reaches here, because the plan reports it instead.
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
    let read = format!(
        "(unsafe {{ &*(ptr as *const {inner}) }}).{}.clone()",
        accessor.field
    );
    let needs_env = ret_needs_env(&accessor.ty);
    let env_param = env_param(needs_env, needs_env);
    let zero = zero_value(&accessor.ty, plan);
    format!(
        "\n#[unsafe(no_mangle)]\npub extern \"system\" fn {symbol}<'local>(\n    {env_param},\n    _class: ::jni::objects::JClass<'local>,\n    ptr: i64,\n){ret} {{\n    let __out = {read};\n    {}\n}}\n",
        returned("__out", &accessor.ty, plan, &zero),
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
                Transfer::Text { .. } | Transfer::Buffer { .. }
            )
        });
    (needs_env, needs_env)
}

fn ret_needs_env(ty: &Ty) -> bool {
    matches!(ty, Ty::Str) || types::element(ty).is_some()
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
        if pinned == Some(param.name.as_str()) {
            continue;
        }
        out.push_str(&param_prelude(param, plan, krate, &zero));
    }
    if let Some(name) = pinned {
        let param = function
            .params
            .iter()
            .find(|p| p.name == name)
            .expect("named by pinned_param");
        out.push_str(&pin_stmt(param, plan, &zero));
    }

    let call = call_expr(function, owner, krate, plan, pinned);
    let no_value = function.throws.is_none() && function.ret == Ty::Unit;
    if no_value {
        let _ = writeln!(out, "    {call};");
        if let Some(name) = pinned {
            let _ = writeln!(out, "    drop({}_pin);", rust_ident(name));
        }
        return out;
    }

    let _ = writeln!(out, "    let __out = {call};");
    if let Some(name) = pinned {
        let _ = writeln!(out, "    drop({}_pin);", rust_ident(name));
    }
    let _ = writeln!(out, "{}", finish(function, plan, opts));
    out
}

/// Which single buffer parameter, if more than one crosses, gets to stay
/// pinned across the call: `AutoElementsCritical` holds a mutable borrow of
/// `env`, so at most one guard can be alive at a time. The writable one
/// wins, since only it saves two copies rather than one; a lone borrowed
/// buffer wins by default.
fn pinned_param<'a>(function: &'a Function, plan: &BindingPlan) -> Option<&'a str> {
    let buffers: Vec<&Param> = function
        .params
        .iter()
        .filter(|p| {
            matches!(
                Transfer::of(p, plan),
                Transfer::Buffer { borrowed: true, .. }
            )
        })
        .collect();
    buffers
        .iter()
        .find(|p| {
            matches!(
                Transfer::of(p, plan),
                Transfer::Buffer { writable: true, .. }
            )
        })
        .copied()
        .or(if buffers.len() == 1 {
            buffers.first().copied()
        } else {
            None
        })
        .map(|p| p.name.as_str())
}

/// The statement converting one non-pinned parameter into the shape the
/// Rust call needs, or nothing for a parameter that already arrives in it.
/// `zero` is the enclosing call's own zero, for the early return a failed
/// JNI call takes instead of panicking under `panic = "abort"`.
fn param_prelude(param: &Param, plan: &BindingPlan, krate: &str, zero: &str) -> String {
    let name = rust_ident(&param.name);
    match Transfer::of(param, plan) {
        Transfer::Text { .. } => format!(
            "    let {name}: String = match env.get_string(&{name}) {{\n        Ok(v) => v.into(),\n        {}\n    }};\n",
            err_arm(zero),
        ),
        Transfer::Buffer {
            element,
            borrowed: false,
            ..
        } => owned_buffer_stmt(&name, &element, zero),
        Transfer::Buffer { element, .. } => copied_buffer_stmt(&name, &element, zero),
        Transfer::Handle { mirrored: true, .. } => {
            let class = types::snake(&class_name(&param.ty));
            format!("    let {name} = {class}_from_ordinal({name});\n")
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
fn owned_buffer_stmt(name: &str, element: &Ty, zero: &str) -> String {
    let spelling = types::scalar(element).expect("buffer element is scalar");
    let natural = element.render();
    let cast = element_cast("__raw", &natural, spelling.rust);
    let err = err_arm(zero);
    format!(
        "    let __len = match env.get_array_length(&{name}) {{\n        Ok(v) => v as usize,\n        {err}\n    }};\n\
         \x20   let mut __raw = vec![{zero_elem}; __len];\n\
         \x20   match env.get_{java}_array_region(&{name}, 0, &mut __raw) {{\n        Ok(()) => {{}}\n        {err}\n    }}\n\
         \x20   let {name}: Vec<{natural}> = {cast};\n",
        zero_elem = zero_literal(spelling.rust),
        java = spelling.java,
    )
}

/// A borrowed buffer that will not stay pinned, because another one in the
/// same call needs the one live `AutoElementsCritical` guard instead.
fn copied_buffer_stmt(name: &str, element: &Ty, zero: &str) -> String {
    let spelling = types::scalar(element).expect("buffer element is scalar");
    let natural = element.render();
    let cast = if natural == spelling.rust {
        "guard.to_vec()".to_string()
    } else {
        format!("guard.iter().map(|v| *v as {natural}).collect()")
    };
    format!(
        "    let {name}: Vec<{natural}> = {{\n\
         \x20       let result = unsafe {{ env.get_array_elements_critical(&{name}, ::jni::objects::ReleaseMode::NoCopyBack) }};\n\
         \x20       {err_block}\n\
         \x20       let guard = result.unwrap();\n\
         \x20       {cast}\n\
         \x20   }};\n",
        err_block = critical_err_block(zero),
    )
}

fn pin_stmt(param: &Param, plan: &BindingPlan, zero: &str) -> String {
    let name = rust_ident(&param.name);
    let Transfer::Buffer { writable, .. } = Transfer::of(param, plan) else {
        unreachable!("pinned_param only names a buffer parameter")
    };
    let mode = if writable { "CopyBack" } else { "NoCopyBack" };
    let binding = if writable { "let mut" } else { "let" };
    format!(
        "    let result = unsafe {{ env.get_array_elements_critical(&{name}, ::jni::objects::ReleaseMode::{mode}) }};\n\
         \x20   {err_block}\n\
         \x20   {binding} {name}_pin = result.unwrap();\n",
        err_block = critical_err_block(zero),
    )
}

/// The `Err` arm for a critical-array acquisition specifically. Every other
/// fallible call here can throw straight out of its own `Err` arm, but
/// `AutoElementsCritical`'s `Ok` value borrows `env` itself, so the borrow
/// checker won't let `env` be reborrowed to throw until the `Result`
/// holding that borrow is dropped first.
fn critical_err_block(zero: &str) -> String {
    format!(
        "if let Err(ref e) = result {{\n        \
         let pending = matches!(e, ::jni::errors::Error::JavaException);\n        \
         let message = e.to_string();\n        \
         drop(result);\n        \
         __throw_unless_pending(&mut env, pending, message);\n        \
         return {zero};\n    \
         }}"
    )
}

/// The pinned slice or mutable slice a call argument passes, reinterpreting
/// the pinned element type when the Rust side wants its unsigned twin.
fn pinned_slice_expr(name: &str, element: &Ty, writable: bool) -> String {
    let natural = element.render();
    let stored = types::scalar(element).expect("scalar").rust;
    let var = format!("{name}_pin");
    if natural == stored {
        return match writable {
            true => format!("&mut {var}"),
            false => format!("&{var}"),
        };
    }
    match writable {
        true => format!(
            "unsafe {{ ::std::slice::from_raw_parts_mut({var}.as_ptr() as *mut {natural}, {var}.len()) }}"
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

fn element_cast(expr: &str, natural: &str, stored: &str) -> String {
    match natural == stored {
        true => expr.to_string(),
        false => format!("{expr}.into_iter().map(|v| v as {natural}).collect()"),
    }
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
/// crate's manifest is exactly why this can never be a bare `.expect(...)`:
/// there is no `catch_unwind` between here and the JVM.
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
    pinned: Option<&str>,
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

fn call_arg(param: &Param, plan: &BindingPlan, pinned: Option<&str>) -> String {
    let name = rust_ident(&param.name);
    if pinned == Some(param.name.as_str()) {
        let Transfer::Buffer {
            element, writable, ..
        } = Transfer::of(param, plan)
        else {
            unreachable!("pinned_param only names a buffer parameter")
        };
        return pinned_slice_expr(&name, &element, writable);
    }
    if matches!(Transfer::of(param, plan), Transfer::Handle { .. }) {
        return name;
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
            format!("{}_to_ordinal({expr})", types::snake(name))
        }
        Ty::Class(_) => format!("Box::into_raw(Box::new({expr})) as i64"),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(_) => format!(
                "match {expr} {{ Some(v) => Box::into_raw(Box::new(v)) as i64, None => 0i64 }}"
            ),
            other => returned(expr, other, plan, zero),
        },
        ty if types::element(ty).is_some() => array_return_expr(expr, ty, zero),
        _ => scalar_return_expr(expr, ty),
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

fn array_return_expr(expr: &str, ty: &Ty, zero: &str) -> String {
    let spelling = types::element(ty).expect("checked");
    let natural = match ty {
        Ty::Bytes => "u8".to_string(),
        Ty::List(inner) => inner.render(),
        _ => unreachable!("checked by types::element"),
    };
    let cast = element_cast(expr, spelling.rust, &natural);
    let err = err_arm(zero);
    format!(
        "{{\n            let __values: Vec<{rust}> = {cast};\n            \
         let __arr = match env.new_{java}_array(__values.len() as i32) {{\n                Ok(v) => v,\n                {err}\n            }};\n            \
         match env.set_{java}_array_region(&__arr, 0, &__values) {{\n                Ok(()) => {{}}\n                {err}\n            }}\n            \
         __arr.into_raw()\n        }}",
        rust = spelling.rust,
        java = spelling.java,
    )
}

fn zero_value(ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Unit => String::new(),
        Ty::Bool => "false".into(),
        Ty::F32 | Ty::F64 => "0.0".into(),
        Ty::Str => "::std::ptr::null_mut()".into(),
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
