//! C bindings, over a `cdylib` and a header.
//!
//! The backend with no marshaling framework under it: every decision the
//! other two delegate to pyo3 or wasm-bindgen is written out here. That is
//! the point of it, both as the substrate every other foreign function
//! interface can read and as the check that the wrapper model really is
//! language-neutral.

mod glue;
mod header;
mod package;
mod types;

use crate::naming;
use crate::plan::BindingPlan;

/// Words a generated C declaration will not accept as a parameter name.
///
/// The last two are not C's: `handle` is the receiver every method already
/// takes, and `error` the out-parameter every failing call already takes.
const RESERVED: &[&str] = &[
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else",
    "enum", "extern", "float", "for", "goto", "if", "inline", "int", "long", "register",
    "restrict", "return", "short", "signed", "sizeof", "static", "struct", "switch", "typedef",
    "union", "unsigned", "void", "volatile", "while", "handle", "error",
];

/// A Rust parameter name as the C identifier it is declared under.
pub(crate) fn c_ident(name: &str) -> String {
    naming::escape(&types::snake(name), RESERVED)
}
use crate::{BindFileSet, BindOptions};

/// Emit a complete C binding package.
pub(crate) fn emit(plan: &BindingPlan, opts: &BindOptions) -> Result<BindFileSet, String> {
    let mut out = BindFileSet {
        notes: notes(plan),
        ..BindFileSet::default()
    };
    let files = &mut out.files;
    files.insert("Cargo.toml".into(), package::cargo_toml(opts));
    files.insert("README.md".into(), package::readme(plan, opts));
    files.insert("src/lib.rs".into(), glue::render(plan, opts));
    files.insert(format!("{}.h", opts.module), header::render(plan, opts));
    files.insert(format!("{}.pc", opts.package), package::pkg_config(opts));
    files.insert(".gitignore".into(), "target/\n".into());
    Ok(out)
}

/// Shapes C cannot take as precisely as the Rust states them, plus what a
/// caller has to do by hand because C will not do it for them.
fn notes(plan: &BindingPlan) -> Vec<String> {
    let mut out: Vec<String> = plan
        .classes
        .iter()
        .filter(|c| c.variants.is_some() && !c.is_plain_enum())
        .map(|c| {
            format!(
                "{}: an enum carrying data binds as an opaque handle; its \
                 variants are not visible from C",
                c.name
            )
        })
        .collect();
    if plan.classes.iter().any(|c| !c.is_plain_enum()) {
        out.push(
            "C frees nothing on its own; every handle, sequence and string \
             this library returns has a matching `*_free`"
                .into(),
        );
    }
    out.extend(crate::plan::transfer_notes(
        plan,
        crate::BindKind::CAbi.buffer_support(),
    ));
    out
}
