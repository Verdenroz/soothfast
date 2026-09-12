//! Node.js bindings, over napi-rs.

mod glue;
mod package;

use crate::naming;
use crate::plan::BindingPlan;
use crate::{BindFileSet, BindOptions};

/// The napi/napi-derive release the generated glue builds against, unless
/// the `[[bind]]` entry pins another.
pub(crate) const DEFAULT_VERSION: &str = "3";

/// The napi-build release the generated `build.rs` compiles against. Not an
/// API a bound surface can outgrow, so it is not part of `backend_version`.
pub(crate) const DEFAULT_BUILD_VERSION: &str = "2";

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

/// Emit a complete Node.js binding package.
pub(crate) fn emit(plan: &BindingPlan, opts: &BindOptions) -> Result<BindFileSet, String> {
    let mut out = BindFileSet {
        notes: notes(plan),
        ..BindFileSet::default()
    };
    let files = &mut out.files;
    files.insert("Cargo.toml".into(), package::cargo_toml(opts));
    files.insert("build.rs".into(), package::build_rs());
    files.insert("package.json".into(), package::package_json(opts));
    files.insert("README.md".into(), package::readme(plan, opts));
    files.insert("src/lib.rs".into(), glue::render(plan, opts));
    files.insert(
        ".gitignore".into(),
        "target/\nnode_modules/\n*.node\nindex.js\nindex.d.ts\n".into(),
    );
    Ok(out)
}

/// Shapes JavaScript cannot take as precisely as the Rust states them, plus
/// signatures that cost a copy Node did not have to pay.
fn notes(plan: &BindingPlan) -> Vec<String> {
    let mut out = shapes(plan);
    out.extend(crate::plan::transfer_notes(plan));
    out
}

fn shapes(plan: &BindingPlan) -> Vec<String> {
    plan.classes
        .iter()
        .filter(|c| c.variants.is_some() && !c.is_plain_enum())
        .map(|c| {
            format!(
                "{}: an enum carrying data binds as an opaque handle; its \
                 variants are not visible from Node",
                c.name
            )
        })
        .collect()
}
