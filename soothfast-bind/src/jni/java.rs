//! The Java sources under `src/main/java/<package>/`.
//!
//! One final class per handle, holding the pointer a native method needs
//! and nothing else. Every native method is `private static native`; the
//! physical constructor that wraps a raw pointer is package-private, so
//! returning a handle from one class's method into another's never needs
//! more than that. A call that can fail needs no special Java syntax at
//! all: the exception the Rust side throws is unchecked, so it propagates
//! through the native call on its own.
//!
//! `Natives` loads the library and owns the one shared `Cleaner` every
//! handle registers with. Every class declaring a native method carries its
//! own `static { Natives.load(); }`: a public constructor's own native call
//! can run before that constructor's body ever reaches a `Natives.CLEANER`
//! reference, so loading has to happen at class-init instead.

use std::fmt::Write;

use crate::model::{Param, Receiver, Ty};
use crate::plan::{Accessor, BindingPlan, Class, Function};
use crate::{BindOptions, GENERATED_JAVA};

use super::{java_ident, types};

/// Render every `.java` file the package needs, keyed by path relative to
/// the output directory.
pub(crate) fn render(plan: &BindingPlan, opts: &BindOptions) -> Vec<(String, String)> {
    let dir = package_dir(&opts.package);
    let mut out = Vec::new();

    if !plan.classes.is_empty() || !plan.functions.is_empty() {
        out.push((format!("{dir}/Natives.java"), natives_class(opts)));
    }

    for class in &plan.classes {
        let content = match class.is_plain_enum() {
            true => enum_class(class, opts),
            false => handle_class(class, plan, opts),
        };
        out.push((format!("{dir}/{}.java", class.name), content));
    }

    let module = types::module_class(&opts.package);
    if !plan.functions.is_empty() {
        out.push((
            format!("{dir}/{module}.java"),
            module_class(&module, plan, opts),
        ));
    }
    if plan.functions().any(|f| f.throws.is_some()) {
        out.push((
            format!("{dir}/{module}Exception.java"),
            exception_class(&module, opts),
        ));
    }
    out
}

fn package_dir(java_package: &str) -> String {
    format!("src/main/java/{}", java_package.replace('.', "/"))
}

/// A payload-free enum mirrors onto a Java enum, in declaration order: the
/// glue crosses it as that same position, an ordinal, in both directions.
fn enum_class(class: &Class, opts: &BindOptions) -> String {
    let mut variants = String::new();
    for variant in class.variants.iter().flatten() {
        let _ = writeln!(variants, "    {},", variant.name);
    }
    format!(
        "{GENERATED_JAVA}package {};\n\n{}public enum {} {{\n{variants}}}\n",
        opts.package,
        doc(class.doc.as_deref(), ""),
        class.name,
    )
}

/// Loads the native library and holds the one `Cleaner` every handle
/// registers with. A single `Cleaner` runs one background thread for its
/// whole life; a package with several handle classes must not start one
/// per class.
/// A package built by `cargo soothfast bind build` bundles the cdylib inside
/// its own jar, under `natives/<os>-<arch>/`, since a jar is the one thing
/// guaranteed to be on the classpath; `java.library.path` is not. A consumer
/// linking the cdylib their own way (an install script, a system package)
/// still works, since the fallback is plain `System.loadLibrary`.
fn natives_class(opts: &BindOptions) -> String {
    let lib = types::native_lib_name(&opts.package);
    format!(
        "{GENERATED_JAVA}package {};\n\n\
         import java.io.IOException;\n\
         import java.io.InputStream;\n\
         import java.lang.ref.Cleaner;\n\
         import java.nio.file.Files;\n\
         import java.nio.file.Path;\n\
         import java.nio.file.StandardCopyOption;\n\n\
         final class Natives {{\n\
         \x20   static final Cleaner CLEANER = Cleaner.create();\n\n\
         \x20   static {{\n\
         \x20       String libName = System.mapLibraryName(\"{lib}\");\n\
         \x20       String resource = \"/natives/\" + nativeDir() + \"/\" + libName;\n\
         \x20       try (InputStream in = Natives.class.getResourceAsStream(resource)) {{\n\
         \x20           if (in == null) {{\n\
         \x20               System.loadLibrary(\"{lib}\");\n\
         \x20           }} else {{\n\
         \x20               String suffix = libName.contains(\".\")\n\
         \x20                       ? libName.substring(libName.lastIndexOf('.'))\n\
         \x20                       : \"\";\n\
         \x20               Path temp = Files.createTempFile(\"{lib}\", suffix);\n\
         \x20               temp.toFile().deleteOnExit();\n\
         \x20               Files.copy(in, temp, StandardCopyOption.REPLACE_EXISTING);\n\
         \x20               System.load(temp.toAbsolutePath().toString());\n\
         \x20           }}\n\
         \x20       }} catch (IOException e) {{\n\
         \x20           throw new RuntimeException(\"failed to load {lib}\", e);\n\
         \x20       }}\n\
         \x20   }}\n\n\
         \x20   /** Touching this forces the block above to run. */\n\
         \x20   static void load() {{\n\
         \x20   }}\n\n\
         \x20   private static String nativeDir() {{\n\
         \x20       String osName = System.getProperty(\"os.name\").toLowerCase();\n\
         \x20       String os;\n\
         \x20       if (osName.contains(\"mac\")) {{\n\
         \x20           os = \"macos\";\n\
         \x20       }} else if (osName.contains(\"win\")) {{\n\
         \x20           os = \"windows\";\n\
         \x20       }} else {{\n\
         \x20           os = \"linux\";\n\
         \x20       }}\n\
         \x20       String archName = System.getProperty(\"os.arch\").toLowerCase();\n\
         \x20       String arch = archName.equals(\"amd64\") ? \"x86_64\" : archName;\n\
         \x20       return os + \"-\" + arch;\n\
         \x20   }}\n\n\
         \x20   private Natives() {{\n\
         \x20   }}\n\
         }}\n",
        opts.package,
    )
}

fn exception_class(module: &str, opts: &BindOptions) -> String {
    format!(
        "{GENERATED_JAVA}package {};\n\n\
         /** Thrown for every {module} call that returns an error. */\n\
         public final class {module}Exception extends RuntimeException {{\n\
         \x20   public {module}Exception(String message) {{\n\
         \x20       super(message);\n\
         \x20   }}\n\
         }}\n",
        opts.package,
    )
}

fn module_class(module: &str, plan: &BindingPlan, opts: &BindOptions) -> String {
    let mut methods = String::new();
    let mut natives = String::new();
    for function in &plan.functions {
        let (native, public) = method(function, None, plan);
        methods.push_str(&public);
        natives.push_str(&native);
    }
    format!(
        "{GENERATED_JAVA}package {};\n\n\
         public final class {module} {{\n\
         \x20   static {{\n\
         \x20       Natives.load();\n\
         \x20   }}\n\n\
         \x20   private {module}() {{\n\
         \x20   }}\n\
         {methods}\n{natives}}}\n",
        opts.package,
    )
}

/// A final handle class: a pointer, a `Cleaner` safety net registered
/// against the package's one shared `Cleaner`, and one method per bound
/// call. The Rust type behind `ptr` is never named here; it is only ever
/// an opaque number this class and the glue agree on.
///
/// A declared constructor becomes a real Java constructor, but the
/// package-private one wrapping a raw pointer always takes a `Raw` marker
/// too: without it, a Rust ctor whose only parameter happens to map to a
/// bare `long` would collide with the wrapping constructor's own erased
/// signature. A class with no declared constructor has nothing to collide
/// with and skips the marker.
fn handle_class(class: &Class, plan: &BindingPlan, opts: &BindOptions) -> String {
    let name = &class.name;
    let mut methods = String::new();
    let mut natives = String::new();

    if let Some(ctor) = &class.ctor {
        let (native, public) = ctor_block(ctor, class, plan);
        methods.push_str(&public);
        natives.push_str(&native);
    }
    for accessor in &class.accessors {
        let (native, public) = getter(accessor, plan);
        methods.push_str(&public);
        natives.push_str(&native);
    }
    for function in class.methods.iter().chain(class.statics.iter()) {
        let (native, public) = method(function, Some(class), plan);
        methods.push_str(&public);
        natives.push_str(&native);
    }

    let (raw_marker, raw_param) = match class.ctor {
        Some(_) => (raw_marker_block(), ", Raw marker".to_string()),
        None => ("", String::new()),
    };

    format!(
        "{GENERATED_JAVA}package {};\n\n\
         import java.lang.ref.Cleaner;\n\n\
         {}public final class {name} implements AutoCloseable {{\n\
         \x20   static {{\n\
         \x20       Natives.load();\n\
         \x20   }}\n\n\
         \x20   private final long ptr;\n\
         \x20   private final Cleaner.Cleanable cleanable;\n\
         {raw_marker}\n\
         \x20   private static final class State implements Runnable {{\n\
         \x20       private final long ptr;\n\n\
         \x20       State(long ptr) {{\n\
         \x20           this.ptr = ptr;\n\
         \x20       }}\n\n\
         \x20       @Override\n\
         \x20       public void run() {{\n\
         \x20           nativeFree(ptr);\n\
         \x20       }}\n\
         \x20   }}\n\n\
         \x20   {name}(long ptr{raw_param}) {{\n\
         \x20       this.ptr = ptr;\n\
         \x20       this.cleanable = Natives.CLEANER.register(this, new State(ptr));\n\
         \x20   }}\n\n\
         \x20   /** The raw handle, readable only from generated code in this package. */\n\
         \x20   long nativePtr() {{\n\
         \x20       return ptr;\n\
         \x20   }}\n\
         {methods}\n\
         \x20   @Override\n\
         \x20   public void close() {{\n\
         \x20       cleanable.clean();\n\
         \x20   }}\n\n\
         {natives}\
         \x20   private static native void nativeFree(long ptr);\n\
         }}\n",
        opts.package,
        doc(class.doc.as_deref(), ""),
    )
}

/// A marker type with no purpose but to give the pointer-wrapping
/// constructor a signature nothing else can share.
fn raw_marker_block() -> &'static str {
    "\n    static final class Raw {\n        private Raw() {\n        }\n\n        \
     static final Raw INSTANCE = new Raw();\n    }\n"
}

/// The declared constructor, as a real Java constructor delegating to the
/// pointer-wrapping one.
fn ctor_block(ctor: &Function, class: &Class, plan: &BindingPlan) -> (String, String) {
    let native_name = types::native_method_name(&ctor.name);
    let mut java_params = Vec::new();
    let mut native_decl_params = Vec::new();
    let mut native_call_args = Vec::new();
    for param in &ctor.params {
        let pname = java_ident(&param.name);
        java_params.push(format!("{} {pname}", types::java_ty(&param.ty)));
        native_decl_params.push(format!(
            "{} {pname}",
            types::native_java_ty(&param.ty, plan)
        ));
        native_call_args.push(call_arg(param, plan, &pname));
    }
    let native_ret = types::native_java_ty(&ctor.ret, plan);
    let call = format!("{native_name}({})", native_call_args.join(", "));

    let public = format!(
        "\n{}    public {}({}) {{\n        this({call}, Raw.INSTANCE);\n    }}\n",
        doc(ctor.doc.as_deref(), "    "),
        class.name,
        java_params.join(", "),
    );
    let native = format!(
        "    private static native {native_ret} {native_name}({});\n",
        native_decl_params.join(", "),
    );
    (native, public)
}

/// A field read. An exported type held by a field never reaches here; the
/// plan reports it instead, the same as every other backend.
fn getter(accessor: &Accessor, plan: &BindingPlan) -> (String, String) {
    let name = java_ident(&accessor.field);
    let native_name = types::native_method_name(&accessor.field);
    let ty = types::java_ty(&accessor.ty);
    let native_ty = types::native_java_ty(&accessor.ty, plan);
    let body = wrap_returned(&format!("{native_name}(ptr)"), &accessor.ty, plan);
    let public = format!(
        "\n{}    public {ty} {name}() {{\n        {body}\n    }}\n",
        doc(accessor.doc.as_deref(), "    "),
    );
    let native = format!("    private static native {native_ty} {native_name}(long ptr);\n");
    (native, public)
}

/// One exported call: the native declaration plus the public method calling
/// it, kept as a pair so the two can never name a different method.
fn method(function: &Function, owner: Option<&Class>, plan: &BindingPlan) -> (String, String) {
    let name = java_ident(&function.name);
    let native_name = types::native_method_name(&function.name);
    let has_receiver = owner.is_some() && function.receiver != Receiver::None;

    let mut java_params = Vec::new();
    let mut native_decl_params = Vec::new();
    let mut native_call_args = Vec::new();
    if has_receiver {
        native_decl_params.push("long ptr".to_string());
        native_call_args.push("ptr".to_string());
    }
    for param in &function.params {
        let pname = java_ident(&param.name);
        java_params.push(format!("{} {pname}", types::java_ty(&param.ty)));
        native_decl_params.push(format!(
            "{} {pname}",
            types::native_java_ty(&param.ty, plan)
        ));
        native_call_args.push(call_arg(param, plan, &pname));
    }

    let ret = types::java_ty(&function.ret);
    let native_ret = types::native_java_ty(&function.ret, plan);
    let qualifiers = if has_receiver { "" } else { "static " };
    let call = format!("{native_name}({})", native_call_args.join(", "));
    let body = wrap_returned(&call, &function.ret, plan);

    let public = format!(
        "\n{}    public {qualifiers}{ret} {name}({}) {{\n        {body}\n    }}\n",
        doc(function.doc.as_deref(), "    "),
        java_params.join(", "),
    );
    let native = format!(
        "    private static native {native_ret} {native_name}({});\n",
        native_decl_params.join(", "),
    );
    (native, public)
}

/// The expression one parameter becomes at the native call site: a handle
/// crosses as its pointer, a mirrored enum as its ordinal, everything else
/// unchanged.
fn call_arg(param: &Param, plan: &BindingPlan, java_name: &str) -> String {
    match &param.ty {
        Ty::Class(name) if plan.is_mirrored(name) => format!("{java_name}.ordinal()"),
        Ty::Class(_) => format!("{java_name}.nativePtr()"),
        _ => java_name.to_string(),
    }
}

/// The public method's return statement, unwrapping whatever the native
/// call handed back into the type the signature promises.
fn wrap_returned(call: &str, ty: &Ty, plan: &BindingPlan) -> String {
    match ty {
        Ty::Unit => format!("{call};"),
        Ty::Class(name) if plan.is_mirrored(name) => format!("return {name}.values()[{call}];"),
        Ty::Class(name) => format!("return {};", wrap_expr(name, call, plan)),
        Ty::Optional(inner) => match &**inner {
            Ty::Class(name) => format!(
                "long ptr_ = {call};\n        return ptr_ == 0 ? null : {};",
                wrap_expr(name, "ptr_", plan)
            ),
            _ => format!("return {call};"),
        },
        _ => format!("return {call};"),
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
        true => format!("new {name}({call}, {name}.Raw.INSTANCE)"),
        false => format!("new {name}({call})"),
    }
}

fn doc(text: Option<&str>, indent: &str) -> String {
    match text {
        Some(text) => format!("{indent}/** {text} */\n"),
        None => String::new(),
    }
}
