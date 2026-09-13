//! C++ bindings: a header-only RAII wrapper generated over the C backend's
//! own ABI.
//!
//! No macro does the marshaling here either, and like Go this wrapper reads
//! someone else's header instead of writing its own: the package is the C
//! backend's file set plus one `.hpp` that wraps every handle in a
//! `unique_ptr`, every failure in a thrown `Error`, and every plain enum in
//! an `enum class`.

mod glue;
mod package;
pub(crate) mod types;

use crate::plan::BindingPlan;
use crate::{BindFileSet, BindOptions};

/// `opts.package` is a `::`-delimited C++ namespace path here, not a Cargo
/// distribution name, and `opts.module` cannot be trusted either: `[[bind]]`
/// defaults it from `package` with only hyphens replaced, which does
/// nothing for the `::`. Both are replaced with the one name
/// `types::package_name` derives, the same way `cgo::c_opts` does for a Go
/// module path, so the embedded C crate comes out the same as a plain `c`
/// entry for that name would. Callers use this before [`plan::lower`] too,
/// since that is where every C symbol name is actually decided.
pub(crate) fn c_opts(opts: &BindOptions) -> BindOptions {
    let name = types::package_name(&opts.package);
    BindOptions {
        package: name.clone(),
        module: name,
        ..opts.clone()
    }
}

/// Emit a complete C++ binding package.
pub(crate) fn emit(plan: &BindingPlan, opts: &BindOptions) -> Result<BindFileSet, String> {
    let cabi_opts = c_opts(opts);
    let module = cabi_opts.module.clone();
    let mut out = crate::cabi::emit(plan, &cabi_opts)?;
    if let Some(readme) = out.files.get_mut("README.md") {
        readme.push_str(&package::readme_section(&opts.package, &module));
    }
    out.files
        .insert(format!("{module}.hpp"), glue::render(plan, opts, &module));
    out.notes = notes(plan);
    Ok(out)
}

/// Shapes C++ cannot take as precisely as the Rust states them.
fn notes(plan: &BindingPlan) -> Vec<String> {
    let mut out: Vec<String> = plan
        .classes
        .iter()
        .filter(|c| c.variants.is_some() && !c.is_plain_enum())
        .map(|c| {
            format!(
                "{}: an enum carrying data binds as an opaque handle; its \
                 variants are not visible from C++",
                c.name
            )
        })
        .collect();
    out.extend(crate::plan::transfer_notes(plan));
    out
}
