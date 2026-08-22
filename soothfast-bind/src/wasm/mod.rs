//! JavaScript bindings, over wasm-bindgen.

mod glue;
mod package;

use crate::naming;
use crate::plan::BindingPlan;
use crate::{BindFileSet, BindOptions};

/// The wasm-bindgen release the generated glue builds against, unless the
/// `[[bind]]` entry pins another.
pub(crate) const DEFAULT_VERSION: &str = "0.2";

/// Words JavaScript will not accept as a binding.
const RESERVED: &[&str] = &[
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "constructor",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

/// A Rust name as the JavaScript identifier it is exported under.
pub(crate) fn js_ident(name: &str) -> String {
    naming::escape(&naming::camel(name), RESERVED)
}

/// Emit a complete JavaScript binding package.
pub(crate) fn emit(plan: &BindingPlan, opts: &BindOptions) -> Result<BindFileSet, String> {
    let mut out = BindFileSet {
        notes: notes(plan),
        ..BindFileSet::default()
    };
    let files = &mut out.files;
    files.insert("Cargo.toml".into(), package::cargo_toml(plan, opts));
    files.insert("README.md".into(), package::readme(plan, opts));
    files.insert("src/lib.rs".into(), glue::render(plan, opts));
    files.insert(".gitignore".into(), "target/\npkg/\n".into());
    Ok(out)
}

/// Shapes JavaScript cannot take as precisely as the Rust states them.
fn notes(plan: &BindingPlan) -> Vec<String> {
    let mut out: Vec<String> = plan
        .classes
        .iter()
        .filter(|c| c.variants.is_some() && !c.is_plain_enum())
        .map(|c| {
            format!(
                "{}: an enum carrying data binds as an opaque handle; its \
                 variants are not visible from JavaScript",
                c.name
            )
        })
        .collect();
    if plan.has_async() {
        out.push(
            "an async method returns a Promise; the handle stays borrowed \
             until it settles"
                .into(),
        );
    }
    out
}
