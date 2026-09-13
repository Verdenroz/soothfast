//! C# bindings, generated over the C backend's own ABI.
//!
//! The same wrapper-over-C shape as [`crate::cgo`]: `bind gen` writes the
//! `.h`/glue/package trio C already gets, then a `.csproj` and the C#
//! sources that call into it through P/Invoke. Unlike cgo, the calling
//! convention has no macro of its own either (`[DllImport]` is plain
//! metadata, not code generation), so every extern declaration is written
//! out here the same way the C backend writes its own header.

mod glue;
mod package;
pub(crate) mod types;

use crate::naming;
use crate::plan::BindingPlan;
use crate::{BindFileSet, BindOptions};

/// Words a generated C# declaration will not accept as a parameter name.
///
/// The last two are not C#'s own: `handle` is the receiver every instance
/// method already takes, and `error` the out-parameter every failing call
/// already takes.
const RESERVED: &[&str] = &[
    "abstract",
    "as",
    "base",
    "bool",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "checked",
    "class",
    "const",
    "continue",
    "decimal",
    "default",
    "delegate",
    "do",
    "double",
    "else",
    "enum",
    "event",
    "explicit",
    "extern",
    "false",
    "finally",
    "fixed",
    "float",
    "for",
    "foreach",
    "goto",
    "if",
    "implicit",
    "in",
    "int",
    "interface",
    "internal",
    "is",
    "lock",
    "long",
    "namespace",
    "new",
    "null",
    "object",
    "operator",
    "out",
    "override",
    "params",
    "private",
    "protected",
    "public",
    "readonly",
    "ref",
    "return",
    "sbyte",
    "sealed",
    "short",
    "sizeof",
    "stackalloc",
    "static",
    "string",
    "struct",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "uint",
    "ulong",
    "unchecked",
    "unsafe",
    "ushort",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
    "handle",
    "error",
];

/// A Rust parameter name as the C# identifier it is declared under.
pub(crate) fn ident(name: &str) -> String {
    naming::escape(name, RESERVED)
}

/// `opts.package` is a dotted namespace here, not a Cargo distribution name.
/// The embedded C crate needs its own single-word name, derived the same way
/// [`crate::cgo::c_opts`] derives Go's: [`types::lib_name`] snake_cases the
/// whole namespace, matching what a plain `c` entry for that name would
/// produce, so the ABI cannot depend on which wrapper reads it.
pub(crate) fn c_opts(opts: &BindOptions) -> BindOptions {
    let name = types::lib_name(&opts.package);
    BindOptions {
        package: name.clone(),
        module: name,
        ..opts.clone()
    }
}

/// Emit a complete C# binding package: the C backend's own files, plus the
/// `.csproj` and the C# sources that call into it.
pub(crate) fn emit(plan: &BindingPlan, opts: &BindOptions) -> Result<BindFileSet, String> {
    let c_opts = c_opts(opts);
    let mut out = crate::cabi::emit(plan, &c_opts)?;
    if let Some(gitignore) = out.files.get_mut(".gitignore") {
        gitignore.push_str("bin/\nobj/\nruntimes/\n");
    }
    if let Some(readme) = out.files.get_mut("README.md") {
        readme.push_str(&package::readme_section(plan, opts));
    }
    let lib = c_opts.module.clone();
    out.files.insert(
        format!("{}.csproj", opts.package),
        package::csproj(plan, opts),
    );
    out.files
        .insert("Native.cs".into(), glue::native(plan, opts, &lib));
    for (path, content) in glue::classes(plan, opts, &lib) {
        out.files.insert(path, content);
    }
    out.notes = notes(plan);
    Ok(out)
}

/// Shapes C# cannot take as precisely as the Rust states them, plus what a
/// pinned buffer forecloses.
fn notes(plan: &BindingPlan) -> Vec<String> {
    let mut out: Vec<String> = plan
        .classes
        .iter()
        .filter(|c| c.variants.is_some() && !c.is_plain_enum())
        .map(|c| {
            format!(
                "{}: an enum carrying data binds as an opaque handle; its \
                 variants are not visible from C#",
                c.name
            )
        })
        .collect();
    out.extend(crate::plan::transfer_notes(plan));
    out
}
