//! LuaJIT bindings, generated over the C backend's own ABI.
//!
//! No macro does the marshaling here either: LuaJIT's `ffi` reads a C
//! declaration directly, so the package is the C backend's file set plus one
//! `.lua` module that declares the header's own types through `ffi.cdef`
//! (rendered from the same declaration writer the header uses, so the two
//! cannot drift) and wraps the flat symbols in ordinary Lua tables.

mod glue;
mod package;
mod types;

use crate::plan::BindingPlan;
use crate::{BindFileSet, BindOptions};

/// `opts.package` is a dotted `require` path here, not a Cargo distribution
/// name, and `opts.module` cannot be trusted either: `[[bind]]` defaults it
/// from `package` with only hyphens replaced, which does nothing for a
/// path's dots. Both are replaced with the one name `types::module_name`
/// derives, the same way `cgo::c_opts` sanitizes a Go module path, so the
/// embedded C crate comes out the same as a plain `c` entry for that name
/// would.
pub(crate) fn c_opts(opts: &BindOptions) -> BindOptions {
    let name = types::module_name(&opts.package);
    BindOptions {
        package: name.clone(),
        module: name,
        ..opts.clone()
    }
}

/// Emit a complete LuaJIT binding package.
pub(crate) fn emit(plan: &BindingPlan, opts: &BindOptions) -> Result<BindFileSet, String> {
    let cabi_opts = c_opts(opts);
    let module = cabi_opts.module.clone();
    let mut out = crate::cabi::emit(plan, &cabi_opts)?;
    if let Some(readme) = out.files.get_mut("README.md") {
        readme.push_str(&package::readme_section(&module));
    }
    out.files.insert(
        types::require_path(&opts.package),
        glue::render(plan, &module),
    );
    out.notes = notes(plan);
    Ok(out)
}

/// Shapes LuaJIT cannot take as precisely as the Rust states them.
fn notes(plan: &BindingPlan) -> Vec<String> {
    let mut out: Vec<String> = plan
        .classes
        .iter()
        .filter(|c| c.variants.is_some() && !c.is_plain_enum())
        .map(|c| {
            format!(
                "{}: an enum carrying data binds as an opaque handle; its \
                 variants are not visible from Lua",
                c.name
            )
        })
        .collect();
    out.extend(crate::plan::transfer_notes(plan));
    out
}
