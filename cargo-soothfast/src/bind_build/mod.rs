//! Shelling out to each language's own packaging tool.
//!
//! `maturin` drives its own build through `pyo3-build-config`, which knows
//! which interpreter to link against; `wasm-pack` drives cargo plus
//! wasm-bindgen's post-processor; `napi build` drives cargo plus its own
//! `.node`/`.d.ts`/JS-loader generation. C has no such tool, so that one is a
//! plain `cargo build`: the glue crate already declares both library kinds.
//! Go rides on that same `cargo build`, then verifies the wrapper compiles
//! against it with `go vet`/`go build`. Ruby rides `bundle exec rake
//! compile` (rb_sys's own `cargo build` wrapper) and then `gem build`; both
//! are independent skip points; a machine with neither installed reports
//! both rather than failing on the first. Lua rides that same `cargo
//! build` too, then verifies the wrapper the same way Go does, except the
//! check runs the module rather than compiling one.

use std::path::Path;

use soothfast_bind::BindKind;

mod cargo_build;
mod cpp;
mod dotnet;
mod go;
mod jvm;
mod lua;
mod node;
mod python;
mod r;
mod ruby;
mod staging;
#[cfg(test)]
mod test_support;
mod wasm;

pub(crate) use cpp::cpp_compiler;

/// Build one entry's package, returning the artifacts it produced. `quiet`
/// silences each tool's own stdout, for a caller (`bind bench`) whose own
/// stdout must carry nothing else.
pub(crate) fn run(
    kind: BindKind,
    glue: &Path,
    targets: &[String],
    release: bool,
    quiet: bool,
) -> Result<Vec<String>, String> {
    match kind {
        BindKind::Python => python::maturin(glue, targets, release, quiet),
        BindKind::Wasm => wasm::wasm_pack(glue, targets, release, quiet),
        BindKind::Node => node::napi(glue, targets, release, quiet),
        BindKind::CAbi => cargo_build::cargo(glue, targets, release, quiet),
        BindKind::Go => go::go(glue, targets, release, quiet),
        BindKind::Java => jvm::jvm(glue, targets, release, jvm::JAVA, quiet),
        BindKind::Kotlin => jvm::jvm(glue, targets, release, jvm::KOTLIN, quiet),
        BindKind::R => r::r(glue, targets, quiet),
        BindKind::Ruby => ruby::ruby(glue, targets, quiet),
        BindKind::Cpp => cpp::cpp(glue, targets, release, quiet),
        BindKind::Lua => lua::lua(glue, targets, release, quiet),
        BindKind::CSharp => dotnet::csharp(glue, targets, release, quiet),
    }
}
