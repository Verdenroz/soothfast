//! The Kotlin sources under `src/main/kotlin/<package>/`.
//!
//! Renders the same plan Java does, over the same JNI glue: a
//! `@JvmStatic external fun` declared in a class's `companion object`
//! compiles to a static native method on that class itself, not on its
//! `Companion`, so it links against exactly the symbol `glue.rs` emits for
//! a Java `private static native` method of the same name. Free functions
//! need no companion at all — a top-level `external fun` in a file already
//! compiles to a static native method on that file's own class.
//!
//! One final class per handle, holding the pointer a native method needs
//! and nothing else. `Natives` loads the library and owns the one shared
//! `Cleaner` every handle registers with; every class declaring a native
//! method forces it to load from its own `companion object` `init` block,
//! which runs during that class's `<clinit>`, before any constructor of it
//! can run.

use std::fmt::Write;

use crate::model::{Param, Ty};
use crate::plan::{Accessor, BindingPlan, Class, Function};
use crate::{BindOptions, GENERATED_JAVA};

use super::{kotlin_ident, types};

/// Render every `.kt` file the package needs, keyed by path relative to the
/// output directory.
pub(crate) fn render(plan: &BindingPlan, opts: &BindOptions) -> Vec<(String, String)> {
    let dir = package_dir(&opts.package);
    let mut out = Vec::new();

    if !plan.classes.is_empty() || !plan.functions.is_empty() {
        out.push((format!("{dir}/Natives.kt"), natives_object(opts)));
    }

    for class in &plan.classes {
        let content = match class.is_plain_enum() {
            true => enum_class(class, opts),
            false => handle_class(class, plan, opts),
        };
        out.push((format!("{dir}/{}.kt", class.name), content));
    }

    let module = types::module_class(&opts.package);
    if !plan.functions.is_empty() {
        out.push((
            format!("{dir}/{module}.kt"),
            module_file(&module, plan, opts),
        ));
    }
    if plan.functions().any(|f| f.throws.is_some()) {
        out.push((
            format!("{dir}/{module}Exception.kt"),
            exception_class(&module, opts),
        ));
    }
    out
}

fn package_dir(java_package: &str) -> String {
    format!("src/main/kotlin/{}", java_package.replace('.', "/"))
}

/// A payload-free enum mirrors onto a Kotlin `enum class`, in declaration
/// order: the glue crosses it as that same position, an ordinal, in both
/// directions.
fn enum_class(class: &Class, opts: &BindOptions) -> String {
    let mut variants = String::new();
    for variant in class.variants.iter().flatten() {
        let _ = writeln!(variants, "    {},", variant.name);
    }
    format!(
        "{GENERATED_JAVA}package {}\n\n{}enum class {} {{\n{variants}}}\n",
        opts.package,
        doc(class.doc.as_deref(), ""),
        class.name,
    )
}

/// Loads the native library and holds the one `Cleaner` every handle
/// registers with, the Kotlin twin of Java's `Natives` class. `internal`
/// stands in for Java's package-private: nothing outside this package
/// should reach a handle's raw pointer.
fn natives_object(opts: &BindOptions) -> String {
    let lib = types::native_lib_name(&opts.package);
    format!(
        "{GENERATED_JAVA}package {}\n\n\
         import java.io.IOException\n\
         import java.lang.ref.Cleaner\n\
         import java.nio.file.Files\n\
         import java.nio.file.StandardCopyOption\n\n\
         internal object Natives {{\n\
         \x20   val CLEANER: Cleaner = Cleaner.create()\n\n\
         \x20   init {{\n\
         \x20       val libName = System.mapLibraryName(\"{lib}\")\n\
         \x20       val resource = \"/natives/\" + nativeDir() + \"/\" + libName\n\
         \x20       try {{\n\
         \x20           val input = Natives::class.java.getResourceAsStream(resource)\n\
         \x20           if (input == null) {{\n\
         \x20               System.loadLibrary(\"{lib}\")\n\
         \x20           }} else {{\n\
         \x20               input.use {{ stream ->\n\
         \x20                   val suffix = if (libName.contains('.')) {{\n\
         \x20                       libName.substring(libName.lastIndexOf('.'))\n\
         \x20                   }} else {{\n\
         \x20                       \"\"\n\
         \x20                   }}\n\
         \x20                   val temp = Files.createTempFile(\"{lib}\", suffix)\n\
         \x20                   temp.toFile().deleteOnExit()\n\
         \x20                   Files.copy(stream, temp, StandardCopyOption.REPLACE_EXISTING)\n\
         \x20                   System.load(temp.toAbsolutePath().toString())\n\
         \x20               }}\n\
         \x20           }}\n\
         \x20       }} catch (e: IOException) {{\n\
         \x20           throw RuntimeException(\"failed to load {lib}\", e)\n\
         \x20       }}\n\
         \x20   }}\n\n\
         \x20   /** Touching this forces the block above to run. */\n\
         \x20   fun load() {{\n\
         \x20   }}\n\n\
         \x20   private fun nativeDir(): String {{\n\
         \x20       val osName = System.getProperty(\"os.name\").lowercase()\n\
         \x20       val os = when {{\n\
         \x20           osName.contains(\"mac\") -> \"macos\"\n\
         \x20           osName.contains(\"win\") -> \"windows\"\n\
         \x20           else -> \"linux\"\n\
         \x20       }}\n\
         \x20       val archName = System.getProperty(\"os.arch\").lowercase()\n\
         \x20       val arch = if (archName == \"amd64\") \"x86_64\" else archName\n\
         \x20       return \"$os-$arch\"\n\
         \x20   }}\n\
         }}\n",
        opts.package,
    )
}

fn exception_class(module: &str, opts: &BindOptions) -> String {
    format!(
        "{GENERATED_JAVA}package {}\n\n\
         /** Thrown for every {module} call that returns an error. */\n\
         class {module}Exception(message: String) : RuntimeException(message)\n",
        opts.package,
    )
}

/// Free functions as top-level declarations in `<Module>.kt`. `@file:JvmName`
/// makes the file's own class the one Java (and this glue) already expects;
/// a top-level `external fun` is static by construction, so it needs no
/// companion to hold it.
fn module_file(module: &str, plan: &BindingPlan, opts: &BindOptions) -> String {
    let mut publics = String::new();
    let mut natives = String::new();
    for function in &plan.functions {
        let (native, public) = free_function(function, plan);
        publics.push_str(&public);
        natives.push_str(&native);
    }
    format!(
        "{GENERATED_JAVA}@file:JvmName(\"{module}\")\n\n\
         package {}\n\n\
         private val loadNatives: Unit = Natives.load()\n\
         {natives}{publics}",
        opts.package,
    )
}

fn free_function(function: &Function, plan: &BindingPlan) -> (String, String) {
    let (native, public) = signature(function, false, plan, "");
    (
        format!("private external fun {native}\n"),
        format!("\n{}fun {public}\n", doc(function.doc.as_deref(), "")),
    )
}

/// A final handle class: a pointer, a `Cleaner` safety net registered
/// against the package's one shared `Cleaner`, and one member per bound
/// call. The Rust type behind `ptr` is never named here; it is only ever an
/// opaque number this class and the glue agree on.
///
/// A declared constructor becomes a real secondary constructor delegating
/// to the pointer-wrapping primary one, which always takes a `Raw` marker
/// too: without it, a Rust ctor whose only parameter happens to map to a
/// bare `Long` would collide with the wrapping constructor's own erased
/// signature, the same clash Java disambiguates the same way. A class with
/// no declared constructor has nothing to collide with and skips the
/// marker.
fn handle_class(class: &Class, plan: &BindingPlan, opts: &BindOptions) -> String {
    let name = &class.name;
    let mut natives = String::new();
    let mut factories = String::new();
    let mut members = String::new();

    if let Some(ctor) = &class.ctor {
        let (native, secondary) = ctor_block(ctor, plan);
        natives.push_str(&native);
        members.push_str(&secondary);
    }
    for accessor in &class.accessors {
        let (native, property) = getter(accessor, plan);
        natives.push_str(&native);
        members.push_str(&property);
    }
    for function in &class.methods {
        let (native, public) = method(function, true, plan);
        natives.push_str(&native);
        members.push_str(&public);
    }
    for function in &class.statics {
        let (native, public) = method(function, false, plan);
        natives.push_str(&native);
        factories.push_str(&public);
    }
    let _ = write!(
        natives,
        "\n        @JvmStatic\n        private external fun nativeFree(ptr: Long)\n",
    );

    let (raw_object, raw_param) = match class.ctor {
        Some(_) => ("\n    private object Raw\n", ", marker: Raw".to_string()),
        None => ("", String::new()),
    };

    format!(
        "{GENERATED_JAVA}package {}\n\n\
         import java.lang.ref.Cleaner\n\n\
         {}class {name} private constructor(private val ptr: Long{raw_param}) : AutoCloseable {{\n\
         \x20   companion object {{\n\
         \x20       init {{\n\
         \x20           Natives.load()\n\
         \x20       }}\n\
         {factories}{natives}\
         \x20   }}\n\
         {raw_object}\n\
         \x20   private val cleanable: Cleaner.Cleanable = Natives.CLEANER.register(this, State(ptr))\n\n\
         \x20   internal fun nativePtr(): Long = ptr\n\n\
         \x20   private class State(private val ptr: Long) : Runnable {{\n\
         \x20       override fun run() {{\n\
         \x20           nativeFree(ptr)\n\
         \x20       }}\n\
         \x20   }}\n\
         {members}\n\
         \x20   override fun close() {{\n\
         \x20       cleanable.clean()\n\
         \x20   }}\n\
         }}\n",
        opts.package,
        doc(class.doc.as_deref(), ""),
    )
}

/// The declared constructor, as a real secondary constructor delegating to
/// the pointer-wrapping primary one.
fn ctor_block(ctor: &Function, plan: &BindingPlan) -> (String, String) {
    let (native, params, native_ret, call) = signature_parts(ctor, false, plan);
    let secondary = format!(
        "\n{}    constructor({params}) : this({call}, Raw)\n",
        doc(ctor.doc.as_deref(), "    "),
    );
    let native =
        format!("\n        @JvmStatic\n        private external fun {native}: {native_ret}\n",);
    (native, secondary)
}

/// A field read. An exported type held by a field never reaches here; the
/// plan reports it instead, the same as every other backend.
fn getter(accessor: &Accessor, plan: &BindingPlan) -> (String, String) {
    let name = kotlin_ident(&accessor.field);
    let native_name = types::native_method_name(&accessor.field);
    let ty = types::kotlin_ty(&accessor.ty);
    let native_ty = types::native_kotlin_ty(&accessor.ty, plan);
    let body = wrap_returned(&format!("{native_name}(ptr)"), &accessor.ty, plan);
    let public = format!(
        "\n{}    val {name}: {ty}\n        get() {{\n            {body}\n        }}\n",
        doc(accessor.doc.as_deref(), "    "),
    );
    let native = format!(
        "\n        @JvmStatic\n        private external fun {native_name}(ptr: Long): {native_ty}\n",
    );
    (native, public)
}

/// One exported call: the native declaration plus the public function
/// calling it, kept as a pair so the two can never name a different method.
/// `has_receiver` is threaded in rather than read off `function.receiver`
/// directly, since the caller already knows it from which of `class.methods`
/// or `class.statics` the function came from — and it decides not just
/// whether `ptr` crosses, but where the wrapper lands: an instance method
/// beside `close()`, a static factory inside the `companion object` beside
/// the native declarations, since Kotlin has no other way to give Java a
/// true static method.
fn method(function: &Function, has_receiver: bool, plan: &BindingPlan) -> (String, String) {
    let indent = if has_receiver { "    " } else { "        " };
    let (native, public) = signature(function, has_receiver, plan, indent);
    let doc_text = doc(function.doc.as_deref(), indent);
    (
        format!("\n        @JvmStatic\n        private external fun {native}\n"),
        format!("\n{doc_text}{indent}fun {public}\n"),
    )
}

/// The native declaration and the public signature for one call, as
/// `(native, public)` strings ready to splice after `external fun ` and
/// `fun `/`constructor` respectively. `indent` is the wrapper's own nesting
/// depth: its body sits one level deeper, its closing brace at `indent`.
fn signature(
    function: &Function,
    has_receiver: bool,
    plan: &BindingPlan,
    indent: &str,
) -> (String, String) {
    let (native, params, native_ret, call) = signature_parts(function, has_receiver, plan);
    let ret = types::kotlin_ty(&function.ret);
    let body = wrap_returned(&call, &function.ret, plan);
    let ret_clause = if ret == "Unit" {
        String::new()
    } else {
        format!(": {ret}")
    };
    let name = kotlin_ident(&function.name);
    (
        format!("{native}: {native_ret}"),
        format!("{name}({params}){ret_clause} {{\n{indent}    {body}\n{indent}}}"),
    )
}

/// Everything that depends on a call's parameter list, shared by methods,
/// statics and the constructor: the native declaration's name and
/// parameters, the public parameter list, the native return type, and the
/// native call expression.
fn signature_parts(
    function: &Function,
    has_receiver: bool,
    plan: &BindingPlan,
) -> (String, String, String, String) {
    let native_name = types::native_method_name(&function.name);
    let mut kotlin_params = Vec::new();
    let mut native_decl_params = Vec::new();
    let mut native_call_args = Vec::new();
    if has_receiver {
        native_decl_params.push("ptr: Long".to_string());
        native_call_args.push("ptr".to_string());
    }
    for param in &function.params {
        let pname = kotlin_ident(&param.name);
        kotlin_params.push(format!("{pname}: {}", types::kotlin_ty(&param.ty)));
        native_decl_params.push(format!(
            "{pname}: {}",
            types::native_kotlin_ty(&param.ty, plan)
        ));
        native_call_args.push(call_arg(param, plan, &pname));
    }
    let native = format!("{native_name}({})", native_decl_params.join(", "));
    let native_ret = types::native_kotlin_ty(&function.ret, plan);
    let call = format!("{native_name}({})", native_call_args.join(", "));
    (native, kotlin_params.join(", "), native_ret, call)
}

/// The expression one parameter becomes at the native call site: a handle
/// crosses as its pointer, a mirrored enum as its ordinal, everything else
/// unchanged.
fn call_arg(param: &Param, plan: &BindingPlan, name: &str) -> String {
    match &param.ty {
        Ty::Class(class_name) if plan.is_mirrored(class_name) => format!("{name}.ordinal"),
        Ty::Class(_) => format!("{name}.nativePtr()"),
        _ => name.to_string(),
    }
}

/// The call's body, converting whatever the native call returns into the
/// value the public signature promises. `Unit` needs no `return`; every
/// other shape does.
fn wrap_returned(call: &str, ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Unit => call.to_string(),
        Ty::Class(name) if plan.is_mirrored(name) => format!("return {name}.entries[{call}]"),
        Ty::Class(name) => format!("return {}", wrap_expr(name, call, plan)),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) => format!(
                "val ptr_ = {call}\n            return if (ptr_ == 0L) null else {}",
                wrap_expr(name, "ptr_", plan)
            ),
            _ => format!("return {call}"),
        },
        _ => format!("return {call}"),
    }
}

/// A raw pointer, wrapped into the handle class it belongs to. A class with
/// its own declared constructor needs the `Raw` marker to reach the
/// pointer-wrapping one instead of colliding with the public constructor.
fn wrap_expr(name: &str, call: &str, plan: &BindingPlan) -> String {
    let has_ctor = plan
        .classes
        .iter()
        .any(|c| c.name == name && c.ctor.is_some());
    match has_ctor {
        true => format!("{name}({call}, {name}.Raw)"),
        false => format!("{name}({call})"),
    }
}

fn doc(text: Option<&str>, indent: &str) -> String {
    match text {
        Some(text) => format!("{indent}/** {text} */\n"),
        None => String::new(),
    }
}

/// The `README.md` a Kotlin consumer builds against, the Kotlin twin of the
/// Java backend's own `package::readme`.
pub(crate) fn readme(plan: &BindingPlan, opts: &BindOptions) -> String {
    let lib = types::native_lib_name(&opts.package);
    let module = types::module_class(&opts.package);
    let mut out = format!("# {}\n\n", opts.package);
    if let Some(description) = &opts.description {
        let _ = writeln!(out, "{description}\n");
    }
    let import = format!("{}.{module}", opts.package);
    let _ = write!(
        out,
        "Kotlin bindings for the `{}` crate, generated by \
         [soothfast](https://github.com/Verdenroz/soothfast). Do not edit by hand: \
         run `cargo soothfast bind gen` instead.\n\n\
         ## Build\n\n\
         ```bash\ncargo build --release\n```\n\n\
         That writes `target/release/lib{lib}.so` (`.dylib` on macOS, `{lib}.dll` on \
         Windows). Compile the Kotlin sources against it and put the library on the \
         loader's path:\n\n\
         ```bash\nkotlinc -d out $(find src/main/kotlin -name '*.kt')\njar --create --file {lib}.jar -C out .\njava -Djava.library.path=target/release -cp {lib}.jar YourMainKt\n```\n\n\
         ## Use\n\n\
         ```kotlin\nimport {import}\n```\n\n\
         Every handle class implements `AutoCloseable`; call `close()` (or use \
         Kotlin's `use {{ }}`) to free it deterministically. A `java.lang.ref.Cleaner` \
         frees one you forgot, but not on any schedule you should rely on.\n\n\
         A borrowed buffer parameter (`DoubleArray`, `ByteArray`, ...) is read through \
         `GetPrimitiveArrayCritical`: it crosses without a copy, but the garbage \
         collector cannot run for the length of that one call.\n",
        opts.crate_name,
    );

    if !plan.classes.is_empty() {
        out.push_str("\n## Types\n\n");
        for class in &plan.classes {
            let _ = writeln!(out, "### `{}`\n", class.name);
            if let Some(doc) = &class.doc {
                let _ = writeln!(out, "{doc}\n");
            }
        }
    }
    if !plan.functions.is_empty() {
        let _ = writeln!(out, "## `{module}`\n");
        for function in &plan.functions {
            let _ = writeln!(out, "- `{}`", kotlin_ident(&function.name));
        }
        out.push('\n');
    }
    if !plan.gaps.is_empty() {
        out.push_str("## Not bound\n\n");
        for gap in &plan.gaps {
            let _ = writeln!(out, "- {}", gap.explain());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Ownership, Receiver};
    use crate::plan::BindingPlan;

    fn plan_with_classes(classes: Vec<Class>) -> BindingPlan {
        BindingPlan {
            classes,
            ..BindingPlan::default()
        }
    }

    fn base_fn(name: &str, receiver: Receiver, ret: Ty) -> Function {
        Function {
            symbol: name.into(),
            rust_path: format!("acme::Widget::{name}"),
            name: name.into(),
            receiver,
            params: Vec::new(),
            ret,
            throws: None,
            is_async: false,
            doc: None,
        }
    }

    fn base_class(name: &str) -> Class {
        Class {
            rust_path: format!("acme::{name}"),
            name: name.into(),
            doc: None,
            send: true,
            sync: true,
            ctor: None,
            accessors: Vec::new(),
            methods: Vec::new(),
            statics: Vec::new(),
            variants: None,
        }
    }

    fn opts() -> BindOptions {
        BindOptions {
            package: "acme.core".into(),
            crate_name: "acme".into(),
            ..BindOptions::default()
        }
    }

    #[test]
    fn a_static_factory_is_a_companion_member_not_an_instance_one() {
        let mut class = base_class("Widget");
        class.statics = vec![base_fn("spawn", Receiver::None, Ty::Class("Widget".into()))];
        let plan = plan_with_classes(vec![class.clone()]);
        let rendered = handle_class(&class, &plan, &opts());
        let (companion, rest) = rendered
            .split_once("private val cleanable")
            .expect("the companion closes before the cleanable field");
        assert!(companion.contains("fun spawn("));
        assert!(companion.contains("private external fun nativeSpawn("));
        assert!(
            !rest.contains("fun spawn("),
            "a static factory must not also render as an instance member"
        );
    }

    #[test]
    fn an_optional_class_return_becomes_a_nullable_type() {
        let mut class = base_class("Widget");
        class.methods = vec![base_fn(
            "next",
            Receiver::Shared,
            Ty::Optional(Box::new(Ty::Class("Widget".into()))),
        )];
        let plan = plan_with_classes(vec![class.clone()]);
        let rendered = handle_class(&class, &plan, &opts());
        assert!(rendered.contains("fun next(): Widget? {"));
        assert!(rendered.contains("if (ptr_ == 0L) null else Widget(ptr_)"));
    }

    #[test]
    fn every_external_fun_name_matches_a_java_style_symbol_it_can_link_against() {
        let mut class = base_class("Widget");
        class.ctor = Some(base_fn("new", Receiver::None, Ty::Class("Widget".into())));
        class.methods = vec![base_fn("bump", Receiver::Exclusive, Ty::I64)];
        let plan = plan_with_classes(vec![class.clone()]);
        let rendered = handle_class(&class, &plan, &opts());
        for native in ["nativeNew", "nativeBump", "nativeFree"] {
            assert!(
                rendered.contains(&format!("private external fun {native}(")),
                "missing {native} in: {rendered}"
            );
        }
    }

    #[test]
    fn a_borrowed_param_crosses_unchanged() {
        let param = crate::model::Param {
            name: "value".into(),
            ty: Ty::I64,
            ownership: Ownership::Borrowed,
        };
        let plan = BindingPlan::default();
        assert_eq!(call_arg(&param, &plan, "value"), "value");
    }
}
