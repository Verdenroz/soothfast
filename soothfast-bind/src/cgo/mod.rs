//! Go bindings (cgo), generated over the C backend's own ABI.
//!
//! No macro does the marshaling here either, but unlike C this wrapper reads
//! someone else's header instead of writing its own: the package is the C
//! backend's file set plus a `go.mod` and one `.go` file that `import "C"`
//! compiles against it.

mod glue;
mod package;
mod types;

use crate::plan::BindingPlan;
use crate::{BindFileSet, BindOptions};

/// `opts.package` is a Go module path here, not a Cargo distribution name,
/// and `opts.module` cannot be trusted either: `[[bind]]` defaults it from
/// `package` with only hyphens replaced, which does nothing for a path's
/// dots and slashes. Both are replaced with the one name `types::package_name`
/// derives, so the embedded C crate comes out the same as a plain `c` entry
/// for that name would: the ABI cannot depend on which wrapper reads it.
/// Callers use this before [`plan::lower`] too, since that is where every C
/// symbol name is actually decided.
pub(crate) fn c_opts(opts: &BindOptions) -> BindOptions {
    let name = types::package_name(&opts.package);
    BindOptions {
        package: name.clone(),
        module: name,
        ..opts.clone()
    }
}

/// Emit a complete Go binding package.
pub(crate) fn emit(plan: &BindingPlan, opts: &BindOptions) -> Result<BindFileSet, String> {
    let cabi_opts = c_opts(opts);
    let package = cabi_opts.package.clone();
    let mut out = crate::cabi::emit(plan, &cabi_opts)?;
    if let Some(readme) = out.files.get_mut("README.md") {
        readme.push_str(&go_readme_section(&package));
    }
    out.files.insert("go.mod".into(), package::go_mod(opts));
    out.files
        .insert(format!("{package}.go"), glue::render(plan, &package));
    out.notes = notes(plan);
    Ok(out)
}

fn go_readme_section(package: &str) -> String {
    format!(
        "\n## Go\n\n\
         This package also ships a Go wrapper (`go.mod`, `{package}.go`) \
         generated over the same header and library. Build the library \
         first, then `go build`/`go test` against it with `CGO_ENABLED=1`.\n\n\
         The `-Wl,-rpath` link flag is untested on Windows.\n"
    )
}

/// Shapes Go cannot take as precisely as the Rust states them.
fn notes(plan: &BindingPlan) -> Vec<String> {
    let mut out: Vec<String> = plan
        .classes
        .iter()
        .filter(|c| c.variants.is_some() && !c.is_plain_enum())
        .map(|c| {
            format!(
                "{}: an enum carrying data binds as an opaque handle; its \
                 variants are not visible from Go",
                c.name
            )
        })
        .collect();
    out.extend(crate::plan::transfer_notes(plan));
    out
}
