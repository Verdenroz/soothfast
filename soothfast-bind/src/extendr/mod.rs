//! R bindings, over extendr.
//!
//! R's numeric and raw vectors are contiguous and its collector does not
//! move objects, so a borrowed one crosses zero-copy, the same answer as
//! Python, C and Node. Where R has no exact type at all — a 64-bit integer,
//! an `Option` of a sequence or a handle — the glue does the conversion by
//! hand instead of leaning on extendr's own derive; see [`glue`] and
//! [`types`].

mod glue;
mod package;
mod types;

use crate::model::Ty;
use crate::plan::BindingPlan;
use crate::{BindFileSet, BindOptions};

/// Emit a complete R binding package: the extendr glue crate plus the R
/// package wrapping it.
pub(crate) fn emit(plan: &BindingPlan, opts: &BindOptions) -> Result<BindFileSet, String> {
    let lib = types::lib_name(&opts.package);
    let mut out = BindFileSet {
        notes: notes(plan),
        ..BindFileSet::default()
    };
    let files = &mut out.files;
    files.insert("DESCRIPTION".into(), package::description(opts));
    files.insert("NAMESPACE".into(), package::namespace(plan, opts));
    files.insert(format!("R/{}.R", opts.package), package::r_wrappers(plan));
    files.insert("README.md".into(), package::readme(plan, opts));
    files.insert("src/entrypoint.c".into(), package::entrypoint_c(&lib));
    files.insert("src/Makevars".into(), package::makevars(&lib));
    files.insert("src/Makevars.win".into(), package::makevars_win(&lib));
    files.insert(
        "src/rust/Cargo.toml".into(),
        package::cargo_toml(opts, &lib),
    );
    files.insert("src/rust/src/lib.rs".into(), glue::render(plan, opts));
    files.insert(
        ".gitignore".into(),
        "src/*.o\nsrc/*.so\nsrc/*.dll\nsrc/*.dylib\nsrc/rust/target/\ntarget/\n".into(),
    );
    files.insert(
        ".Rbuildignore".into(),
        "^src/rust/target$\n^target$\n^\\.gitignore$\n".into(),
    );
    Ok(out)
}

/// Shapes R cannot take as precisely as the Rust states them, plus what a
/// caller has to accept because R's own numeric types cannot carry it.
fn notes(plan: &BindingPlan) -> Vec<String> {
    let mut out: Vec<String> = plan
        .classes
        .iter()
        .filter(|c| c.variants.is_some() && !c.is_plain_enum())
        .map(|c| {
            format!(
                "{}: an enum carrying data binds as an opaque handle; its \
                 variants are not visible from R",
                c.name
            )
        })
        .collect();
    if needs_range_note(plan) {
        out.push(
            "R has no 64-bit integer: a 64-bit or platform-width value \
             crosses as a double, checked on the way in and rejected if it \
             carries a fraction or falls outside the target type's range"
                .into(),
        );
    }
    out.extend(crate::plan::transfer_notes(plan));
    out
}

/// Whether anything in the plan crosses as a range-checked double: a
/// parameter, a return, or a field read all deserve the note, even though
/// only a parameter ever needs the checked conversion itself.
fn needs_range_note(plan: &BindingPlan) -> bool {
    plan.functions()
        .any(|f| f.params.iter().any(|p| range_checked(&p.ty)) || range_checked(&f.ret))
        || plan
            .classes
            .iter()
            .flat_map(|c| c.accessors.iter())
            .any(|a| range_checked(&a.ty))
}

fn range_checked(ty: &Ty) -> bool {
    types::checked_int(ty).is_some()
        || matches!(ty, Ty::List(inner) if types::checked_int(inner).is_some())
}
