//! The P/Invoke layer (`Native.cs`) and the public API built over it.
//!
//! Every exported item gets a `[DllImport]` extern in `Native.cs` under the
//! flat symbol the plan already assigns, the same symbol the C header
//! declares. The public class wrapping it pins a buffer or an encoded
//! string with `fixed` for the one call that needs the pointer, then copies
//! anything the call hands back before freeing it through the same C `free`
//! every other backend calls.

use crate::cabi::glue::{arrays, returns_text};
use crate::cabi::types as c;
use crate::model::{Receiver, Ty};
use crate::plan::{Accessor, BindingPlan, Class, Function, Transfer};
use crate::{BindOptions, GENERATED_CSHARP};

use super::types;

/// Render `Native.cs`: the array structs P/Invoke returns by value, and one
/// `[DllImport]` extern per C symbol the plan needs.
pub(crate) fn native(plan: &BindingPlan, opts: &BindOptions, module: &str) -> String {
    let mut out = format!(
        "{GENERATED_CSHARP}\nusing System;\nusing System.IO;\nusing System.Reflection;\n\
         using System.Runtime.CompilerServices;\nusing System.Runtime.InteropServices;\n\n\
         namespace {};\n",
        opts.package,
    );
    for (ty, _) in arrays(plan) {
        out.push_str(&array_struct(&ty, module));
    }
    out.push_str(&format!(
        "\ninternal static class Native\n{{\n    private const string LibraryName = \"{module}\";\n\n"
    ));
    out.push_str(RESOLVER);
    if returns_text(plan) {
        out.push_str(&string_free_decl(module));
    }
    for (ty, _) in arrays(plan) {
        out.push_str(&array_free_decl(&ty, module));
    }
    for class in &plan.classes {
        out.push_str(&class_native_decls(class, plan, module));
    }
    for function in &plan.functions {
        out.push_str(&native_decl(function, None, plan, module));
    }
    out.push_str("}\n");
    out
}

fn array_struct(ty: &Ty, module: &str) -> String {
    let name = c::array_rust(ty, module);
    format!("\ninternal struct {name}\n{{\n    public IntPtr Data;\n    public nuint Len;\n}}\n")
}

fn array_free_decl(ty: &Ty, module: &str) -> String {
    let name = c::array_rust(ty, module);
    let free = format!("{}_free", c::array_c(ty, module));
    format!(
        "\n    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]\n    \
         internal static extern void {free}({name} array);\n"
    )
}

fn string_free_decl(module: &str) -> String {
    format!(
        "\n    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]\n    \
         internal static extern void {module}_string_free(IntPtr text);\n"
    )
}

fn class_native_decls(class: &Class, plan: &BindingPlan, module: &str) -> String {
    if class.is_plain_enum() {
        return String::new();
    }
    let handle = c::handle_c(&class.name, module);
    let mut out = format!(
        "\n    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]\n    \
         internal static extern void {handle}_free(IntPtr handle);\n"
    );
    if let Some(ctor) = &class.ctor {
        out.push_str(&native_decl(ctor, Some(class), plan, module));
    }
    for accessor in &class.accessors {
        out.push_str(&native_getter_decl(accessor, class, plan, module));
    }
    for function in class.methods.iter().chain(class.statics.iter()) {
        out.push_str(&native_decl(function, Some(class), plan, module));
    }
    out
}

fn native_getter_decl(
    accessor: &Accessor,
    class: &Class,
    plan: &BindingPlan,
    module: &str,
) -> String {
    let handle = c::handle_c(&class.name, module);
    let symbol = format!("{handle}_{}", c::snake(&accessor.field));
    let ret = types::native_ty(&accessor.ty, plan, module);
    format!(
        "\n    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]\n    \
         internal static extern {ret} {symbol}(IntPtr handle);\n"
    )
}

/// One `[DllImport]` extern: the receiver (if any), one or two arguments per
/// parameter, then the error out-parameter a failing call writes through.
fn native_decl(
    function: &Function,
    owner: Option<&Class>,
    plan: &BindingPlan,
    module: &str,
) -> String {
    let (params, needs_unsafe) = native_params(function, owner, plan);
    let params = params.join(", ");
    let ret = types::native_ty(&function.ret, plan, module);
    let modifier = match needs_unsafe {
        true => "extern unsafe",
        false => "extern",
    };
    format!(
        "\n    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]\n    \
         internal static {modifier} {ret} {}({params});\n",
        function.symbol,
    )
}

fn native_params(
    function: &Function,
    owner: Option<&Class>,
    plan: &BindingPlan,
) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut needs_unsafe = false;
    if let (Some(_), receiver) = (owner, function.receiver)
        && receiver != Receiver::None
    {
        out.push("IntPtr handle".to_string());
    }
    for param in &function.params {
        let name = super::ident(&param.name);
        match Transfer::of(param, plan) {
            Transfer::Buffer { element, .. } => {
                let native = types::scalar_of(element).native;
                out.push(format!("{native}* {name}"));
                out.push(format!("nuint {name}_len"));
                needs_unsafe = true;
            }
            Transfer::Text { .. } => {
                out.push(format!("byte* {name}"));
                needs_unsafe = true;
            }
            Transfer::Handle { mirrored: true, .. } => out.push(format!("int {name}")),
            Transfer::Handle { .. } => out.push(format!("IntPtr {name}")),
            _ => out.push(format!(
                "{} {name}",
                types::scalar(&param.ty)
                    .map(|s| s.native)
                    .unwrap_or_default()
            )),
        }
    }
    if function.throws.is_some() {
        out.push("out IntPtr error".to_string());
    }
    (out, needs_unsafe)
}

/// The module initializer and the resolver it wires up, identical for every
/// package regardless of the plan: only `LibraryName`, declared above this,
/// varies.
const RESOLVER: &str = r#"    [ModuleInitializer]
    internal static void Init()
    {
        NativeLibrary.SetDllImportResolver(typeof(Native).Assembly, Resolve);
    }

    private static IntPtr Resolve(string libraryName, Assembly assembly, DllImportSearchPath? searchPath)
    {
        if (libraryName != LibraryName)
        {
            return IntPtr.Zero;
        }
        string fileName = OperatingSystem.IsWindows()
            ? $"{LibraryName}.dll"
            : OperatingSystem.IsMacOS()
                ? $"lib{LibraryName}.dylib"
                : $"lib{LibraryName}.so";
        bool arm64 = RuntimeInformation.ProcessArchitecture == Architecture.Arm64;
        string rid = OperatingSystem.IsWindows()
            ? (arm64 ? "win-arm64" : "win-x64")
            : OperatingSystem.IsMacOS()
                ? (arm64 ? "osx-arm64" : "osx-x64")
                : (arm64 ? "linux-arm64" : "linux-x64");
        string baseDir = AppContext.BaseDirectory;
        foreach (string candidate in new[]
        {
            Path.Combine(baseDir, "runtimes", rid, "native", fileName),
            Path.Combine(baseDir, fileName),
        })
        {
            if (File.Exists(candidate) && NativeLibrary.TryLoad(candidate, out IntPtr handle))
            {
                return handle;
            }
        }
        return NativeLibrary.TryLoad(LibraryName, assembly, searchPath, out IntPtr fallback)
            ? fallback
            : IntPtr.Zero;
    }
"#;

/// Every `.cs` file the package needs besides `Native.cs`, keyed by path
/// relative to the output directory.
pub(crate) fn classes(
    plan: &BindingPlan,
    opts: &BindOptions,
    module: &str,
) -> Vec<(String, String)> {
    let module_class = types::module_class(&opts.package);
    let exception = types::exception_class(&module_class);
    let mut out = Vec::new();

    for class in &plan.classes {
        let content = match class.is_plain_enum() {
            true => mirrored_enum(class, opts),
            false => handle_class(class, plan, opts, module, &exception),
        };
        out.push((format!("{}.cs", class.name), content));
    }
    if !plan.functions.is_empty() {
        out.push((
            format!("{module_class}.cs"),
            module_file(plan, opts, module, &module_class, &exception),
        ));
    }
    if plan.functions().any(|f| f.throws.is_some()) {
        out.push((
            format!("{exception}.cs"),
            exception_file(opts, module, &exception),
        ));
    }
    out
}

fn mirrored_enum(class: &Class, opts: &BindOptions) -> String {
    let variants: String = class
        .variants
        .iter()
        .flatten()
        .map(|v| format!("    {},\n", v.name))
        .collect();
    format!(
        "{GENERATED_CSHARP}\nnamespace {};\n\n{}public enum {}\n{{\n{variants}}}\n",
        opts.package,
        doc(class.doc.as_deref()),
        class.name,
    )
}

/// A handle class: a `SafeHandle` releasing it through the C `*_free`, an
/// internal constructor wrapping a pointer another call already returned,
/// and the declared constructor (if any) as a real one.
fn handle_class(
    class: &Class,
    plan: &BindingPlan,
    opts: &BindOptions,
    module: &str,
    exception: &str,
) -> String {
    let mut members = String::new();
    if let Some(ctor_fn) = &class.ctor {
        members.push_str(&ctor(ctor_fn, class, plan, module, exception));
    }
    members.push_str(&format!(
        "\ninternal {}(IntPtr ptr)\n{{\n    _handle = new Handle(ptr);\n}}\n",
        class.name
    ));
    for accessor in &class.accessors {
        members.push_str(&getter(accessor, class, plan, module));
    }
    for function in class.methods.iter().chain(class.statics.iter()) {
        members.push_str(&method(function, Some(class), plan, module, exception));
    }

    format!(
        "{GENERATED_CSHARP}\nusing System;\nusing System.Runtime.InteropServices;\n\
         using Microsoft.Win32.SafeHandles;\n\n\
         namespace {};\n\n\
         {}public sealed class {} : IDisposable\n\
         {{\n\
         \x20   private sealed class Handle : SafeHandleZeroOrMinusOneIsInvalid\n\
         \x20   {{\n\
         \x20       internal Handle(IntPtr ptr) : base(true)\n\
         \x20       {{\n\
         \x20           SetHandle(ptr);\n\
         \x20       }}\n\n\
         \x20       protected override bool ReleaseHandle()\n\
         \x20       {{\n\
         \x20           Native.{}_free(handle);\n\
         \x20           return true;\n\
         \x20       }}\n\
         \x20   }}\n\n\
         \x20   private readonly Handle _handle;\n\n\
         \x20   internal IntPtr NativeHandle => _handle.DangerousGetHandle();\n\
         {}\n\
         \x20   public void Dispose()\n\
         \x20   {{\n\
         \x20       _handle.Dispose();\n\
         \x20   }}\n\
         }}\n",
        opts.package,
        doc(class.doc.as_deref()),
        class.name,
        c::handle_c(&class.name, module),
        indent(&members, 4),
    )
}

fn module_file(
    plan: &BindingPlan,
    opts: &BindOptions,
    module: &str,
    module_class: &str,
    exception: &str,
) -> String {
    let mut methods = String::new();
    for function in &plan.functions {
        methods.push_str(&method(function, None, plan, module, exception));
    }
    format!(
        "{GENERATED_CSHARP}\nusing System;\nusing System.Runtime.InteropServices;\n\n\
         namespace {};\n\npublic static class {module_class}\n{{\n{}}}\n",
        opts.package,
        indent(&methods, 4),
    )
}

fn exception_file(opts: &BindOptions, module: &str, exception: &str) -> String {
    format!(
        "{GENERATED_CSHARP}\nusing System;\nusing System.Runtime.InteropServices;\n\n\
         namespace {};\n\n\
         /// Thrown for every call this package's native library reports as failed.\n\
         public sealed class {exception} : Exception\n\
         {{\n\
         \x20   internal {exception}(string message) : base(message)\n\
         \x20   {{\n\
         \x20   }}\n\n\
         \x20   internal static {exception} FromNative(IntPtr error)\n\
         \x20   {{\n\
         \x20       string message = Marshal.PtrToStringUTF8(error) ?? string.Empty;\n\
         \x20       Native.{module}_string_free(error);\n\
         \x20       return new {exception}(message);\n\
         \x20   }}\n\
         }}\n",
        opts.package,
    )
}

/// The declared constructor, as a real C# constructor.
fn ctor(
    function: &Function,
    class: &Class,
    plan: &BindingPlan,
    module: &str,
    exception: &str,
) -> String {
    let params = public_params(function, plan);
    let (prep, pins, call) = build_call(function, None, plan);
    let inner = match function.throws.is_some() {
        false => format!("_handle = new Handle({call});\n"),
        true => format!(
            "{} result = {call};\n{}_handle = new Handle(result);\n",
            types::native_ty(&function.ret, plan, module),
            throw_check(exception),
        ),
    };
    let body = wrap_pins(prep, pins, inner);
    format!(
        "\n{}public {}({params})\n{{\n{}}}\n",
        doc(function.doc.as_deref()),
        class.name,
        indent(&body, 4),
    )
}

/// A field read. An exported type held by a field never reaches here; the
/// plan reports it instead, the same as every other backend.
fn getter(accessor: &Accessor, class: &Class, plan: &BindingPlan, module: &str) -> String {
    let name = c::pascal(&accessor.field);
    let symbol = format!(
        "{}_{}",
        c::handle_c(&class.name, module),
        c::snake(&accessor.field)
    );
    let native_ty = types::native_ty(&accessor.ty, plan, module);
    let public_ty = types::public_ty(&accessor.ty);
    let call = format!("Native.{symbol}(NativeHandle)");
    let body = format!(
        "{native_ty} result = {call};\n{}",
        convert_and_return(&accessor.ty, plan, module, "result"),
    );
    format!(
        "\n{}public {public_ty} {name}\n{{\n    get\n    {{\n{}    }}\n}}\n",
        doc(accessor.doc.as_deref()),
        indent(&body, 8),
    )
}

/// One exported call, as the public instance or static method calling
/// through `Native`.
fn method(
    function: &Function,
    owner: Option<&Class>,
    plan: &BindingPlan,
    module: &str,
    exception: &str,
) -> String {
    let name = c::pascal(&function.name);
    let ret = types::public_ty(&function.ret);
    let params = public_params(function, plan);
    let qualifiers = match (owner, function.receiver) {
        (Some(_), Receiver::None) | (None, _) => "public static ",
        (Some(_), _) => "public ",
    };
    let body = call_and_return(function, owner, plan, module, exception);
    format!(
        "\n{}{qualifiers}{ret} {name}({params})\n{{\n{}}}\n",
        doc(function.doc.as_deref()),
        indent(&body, 4),
    )
}

fn public_params(function: &Function, plan: &BindingPlan) -> String {
    function
        .params
        .iter()
        .map(|p| {
            let name = super::ident(&p.name);
            let ty = match Transfer::of(p, plan) {
                Transfer::Buffer {
                    element, writable, ..
                } => types::buffer_public_ty(element, writable),
                _ => types::public_ty(&p.ty),
            };
            format!("{ty} {name}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn call_and_return(
    function: &Function,
    owner: Option<&Class>,
    plan: &BindingPlan,
    module: &str,
    exception: &str,
) -> String {
    let (prep, pins, call) = build_call(function, owner, plan);
    let inner = match (&function.ret, function.throws.is_some()) {
        (Ty::Unit, false) => format!("{call};\n"),
        (Ty::Unit, true) => format!("{call};\n{}", throw_check(exception)),
        (ret, false) => format!(
            "{} result = {call};\n{}",
            types::native_ty(ret, plan, module),
            convert_and_return(ret, plan, module, "result"),
        ),
        (ret, true) => format!(
            "{} result = {call};\n{}{}",
            types::native_ty(ret, plan, module),
            throw_check(exception),
            convert_and_return(ret, plan, module, "result"),
        ),
    };
    wrap_pins(prep, pins, inner)
}

/// The receiver (if any) and one argument per parameter, then the error
/// out-parameter a failing call writes through. A buffer or a string needs
/// its pointer pinned for the call, so those come back as separate `fixed`
/// lines the caller wraps the call in rather than plain argument text.
fn build_call(
    function: &Function,
    owner: Option<&Class>,
    plan: &BindingPlan,
) -> (String, Vec<String>, String) {
    let mut prep = String::new();
    let mut pins = Vec::new();
    let mut args = Vec::new();

    if let (Some(_), receiver) = (owner, function.receiver)
        && receiver != Receiver::None
    {
        args.push("NativeHandle".to_string());
    }
    for param in &function.params {
        let name = super::ident(&param.name);
        match Transfer::of(param, plan) {
            Transfer::Buffer { element, .. } => {
                let ptr = format!("{name}Ptr");
                let native = types::scalar_of(element).native;
                pins.push(format!("fixed ({native}* {ptr} = {name})"));
                args.push(format!("{ptr}, (nuint){name}.Length"));
            }
            Transfer::Text { nullable: true, .. } => {
                let bytes = format!("{name}Bytes");
                let ptr = format!("{name}Ptr");
                prep.push_str(&format!(
                    "byte[]? {bytes} = {name} is null ? null : System.Text.Encoding.UTF8.GetBytes({name} + \"\\0\");\n"
                ));
                pins.push(format!("fixed (byte* {ptr} = {bytes})"));
                args.push(ptr);
            }
            Transfer::Text { .. } => {
                let bytes = format!("{name}Bytes");
                let ptr = format!("{name}Ptr");
                prep.push_str(&format!(
                    "byte[] {bytes} = System.Text.Encoding.UTF8.GetBytes({name} + \"\\0\");\n"
                ));
                pins.push(format!("fixed (byte* {ptr} = {bytes})"));
                args.push(ptr);
            }
            Transfer::Handle { mirrored: true, .. } => args.push(format!("(int){name}")),
            Transfer::Handle { .. } => args.push(format!("{name}.NativeHandle")),
            _ if param.ty == Ty::Bool => args.push(format!("(byte)({name} ? 1 : 0)")),
            _ => args.push(name),
        }
    }
    if function.throws.is_some() {
        args.push("out IntPtr error".to_string());
    }
    (
        prep,
        pins,
        format!("Native.{}({})", function.symbol, args.join(", ")),
    )
}

fn wrap_pins(prep: String, pins: Vec<String>, inner: String) -> String {
    let mut out = prep;
    if pins.is_empty() {
        out.push_str(&inner);
        return out;
    }
    // `unsafe` as a statement takes a block, not a bare `fixed` statement, so
    // the fixed chain needs its own braces one level inside it.
    let mut fixed_chain = String::new();
    for pin in &pins {
        fixed_chain.push_str(pin);
        fixed_chain.push('\n');
    }
    fixed_chain.push_str("{\n");
    fixed_chain.push_str(&indent(&inner, 4));
    fixed_chain.push_str("}\n");

    out.push_str("unsafe\n{\n");
    out.push_str(&indent(&fixed_chain, 4));
    out.push_str("}\n");
    out
}

fn throw_check(exception: &str) -> String {
    format!("if (error != IntPtr.Zero)\n{{\n    throw {exception}.FromNative(error);\n}}\n")
}

/// A native call's raw result, converted into the shape the public API
/// promises, then freed if it was heap-allocated: a bool from its byte, a
/// string copied and released, a mirrored enum cast, a handle wrapped
/// (`null` for a nullable one that came back zero), or a sequence copied
/// with `Marshal.Copy` and released.
fn convert_and_return(ty: &Ty, plan: &BindingPlan, module: &str, var: &str) -> String {
    match ty {
        Ty::Bool => format!("return {var} != 0;\n"),
        Ty::Str => format!(
            "string value = Marshal.PtrToStringUTF8({var}) ?? string.Empty;\n\
             Native.{module}_string_free({var});\n\
             return value;\n"
        ),
        Ty::Class(name) if plan.is_mirrored(name) => format!("return ({name}){var};\n"),
        Ty::Class(name) => format!("return {var} == IntPtr.Zero ? null : new {name}({var});\n"),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) => {
                format!("return {var} == IntPtr.Zero ? null : new {name}({var});\n")
            }
            Ty::Str => format!(
                "if ({var} == IntPtr.Zero)\n{{\n    return null;\n}}\n\
                 string value = Marshal.PtrToStringUTF8({var}) ?? string.Empty;\n\
                 Native.{module}_string_free({var});\n\
                 return value;\n"
            ),
            _ => format!("return {var};\n"),
        },
        ty => match types::element(ty) {
            Some(spelling) => {
                let public = spelling.public;
                let free = format!("{}_free", c::array_c(ty, module));
                format!(
                    "{public}[] value = new {public}[{var}.Len];\n\
                     if ({var}.Len > 0)\n\
                     {{\n    Marshal.Copy({var}.Data, value, 0, (int){var}.Len);\n}}\n\
                     Native.{free}({var});\n\
                     return value;\n"
                )
            }
            None => format!("return {var};\n"),
        },
    }
}

fn doc(text: Option<&str>) -> String {
    match text {
        Some(text) => format!("/// {text}\n"),
        None => String::new(),
    }
}

/// Indents every non-empty line of `text` by `spaces`, for nesting a block
/// built independently of the braces it lands inside.
fn indent(text: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    let mut out = String::new();
    for line in text.lines() {
        if !line.is_empty() {
            out.push_str(&pad);
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}
