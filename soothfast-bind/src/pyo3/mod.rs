//! Python bindings, over pyo3.

mod asyncrt;
mod buffers;
mod glue;
mod package;

use crate::naming;
use crate::plan::BindingPlan;
use crate::{BindFileSet, BindOptions};

/// The pyo3 release the generated glue builds against, unless the `[[bind]]`
/// entry pins another.
pub(crate) const DEFAULT_VERSION: &str = "0.26";

const KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

/// A Rust name as a Python identifier.
pub(crate) fn py_ident(name: &str) -> String {
    naming::escape(name, KEYWORDS)
}

/// Emit a complete Python binding package.
pub(crate) fn emit(plan: &BindingPlan, opts: &BindOptions) -> Result<BindFileSet, String> {
    let mut out = BindFileSet {
        notes: notes(plan),
        ..BindFileSet::default()
    };
    let files = &mut out.files;
    files.insert("Cargo.toml".into(), package::cargo_toml(plan, opts));
    files.insert("pyproject.toml".into(), package::pyproject(opts));
    files.insert("README.md".into(), package::readme(plan, opts));
    files.insert("src/lib.rs".into(), glue::render(plan, opts));
    files.insert(".gitignore".into(), "target/\n".into());
    Ok(out)
}

/// Shapes Python cannot take as precisely as the Rust states them, plus
/// signatures that cost a copy Python did not have to pay.
fn notes(plan: &BindingPlan) -> Vec<String> {
    let mut out = shapes(plan);
    out.extend(crate::plan::transfer_notes(
        plan,
        crate::BindKind::Python.buffer_support(),
    ));
    out
}

fn shapes(plan: &BindingPlan) -> Vec<String> {
    plan.classes
        .iter()
        .filter(|c| c.variants.is_some() && !c.is_plain_enum())
        .map(|c| {
            format!(
                "{}: an enum carrying data binds as an opaque handle; its \
                 variants are not visible from Python",
                c.name
            )
        })
        .collect()
}
