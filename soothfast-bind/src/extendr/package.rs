//! The R package: `DESCRIPTION`, `NAMESPACE`, `R/<pkg>.R`, and everything
//! `src/` needs besides the glue crate itself.

use std::fmt::Write;

use crate::plan::{BindingPlan, Class, Function};
use crate::{BindOptions, GENERATED_R, GENERATED_TOML};

use super::types;

/// The extendr-api release the generated glue builds against, unless the
/// `[[bind]]` entry pins another.
pub(crate) const DEFAULT_VERSION: &str = "0.9";

pub(crate) fn description(opts: &BindOptions) -> String {
    let summary = opts
        .description
        .clone()
        .unwrap_or_else(|| format!("R bindings for the `{}` crate", opts.crate_name));
    format!(
        "Package: {}\nType: Package\nTitle: {summary}\nVersion: {}\nDescription: {summary}.\nEncoding: UTF-8\nSystemRequirements: Cargo (Rust's package manager), rustc\n",
        opts.package, opts.version,
    )
}

pub(crate) fn namespace(plan: &BindingPlan, opts: &BindOptions) -> String {
    let mut out = format!(
        "{GENERATED_R}useDynLib({}, .registration = TRUE)\n",
        opts.package
    );
    for function in &plan.functions {
        let _ = writeln!(out, "export({})", types::r_ident(&function.name));
    }
    for class in plan.classes.iter().filter(|c| !c.is_plain_enum()) {
        if class.ctor.is_some() {
            let _ = writeln!(out, "export({})", class.name);
        }
        for s in &class.statics {
            let _ = writeln!(out, "export({})", static_fn_name(class, s));
        }
        if !class.methods.is_empty() || !class.accessors.is_empty() {
            let _ = writeln!(out, "S3method(\"$\", {})", class.name);
        }
    }
    out
}

/// `R/<pkg>.R`: the wrapper functions and the class methods. A plain enum
/// needs none of this — it crosses as a validated string, so the Rust value
/// it mirrors never has an R-side representation of its own.
pub(crate) fn r_wrappers(plan: &BindingPlan) -> String {
    let mut out = String::from(GENERATED_R);
    for function in &plan.functions {
        out.push_str(&free_fn(function, None));
    }
    for class in plan.classes.iter().filter(|c| !c.is_plain_enum()) {
        out.push_str(&class_r(class));
    }
    out
}

/// A free function, or a class static (which extendr also spells with no
/// receiver argument, so the two render identically).
fn free_fn(function: &Function, owner: Option<&Class>) -> String {
    let name = owner
        .map(|c| static_fn_name(c, function))
        .unwrap_or_else(|| types::r_ident(&function.name));
    let params: Vec<String> = function
        .params
        .iter()
        .map(|p| types::r_ident(&p.name))
        .collect();
    let symbol = match owner {
        Some(c) => types::wrap_method(&c.name, &function.name),
        None => types::wrap_fn(&function.name),
    };
    format!(
        "{name} <- function({}) .Call({symbol}, {})\n",
        params.join(", "),
        params.join(", "),
    )
}

fn static_fn_name(class: &Class, function: &Function) -> String {
    format!("{}_{}", class.name, types::r_ident(&function.name))
}

fn instance_helper_name(class: &Class, member: &str) -> String {
    format!("{}__{}", class.name, types::r_ident(member))
}

fn class_r(class: &Class) -> String {
    let mut out = String::new();
    if let Some(ctor) = &class.ctor {
        let params: Vec<String> = ctor
            .params
            .iter()
            .map(|p| types::r_ident(&p.name))
            .collect();
        let symbol = types::wrap_method(&class.name, &ctor.name);
        let _ = writeln!(
            out,
            "{} <- function({}) .Call({symbol}, {})",
            class.name,
            params.join(", "),
            params.join(", "),
        );
    }
    for s in &class.statics {
        out.push_str(&free_fn(s, Some(class)));
    }

    let mut arms = String::new();
    for accessor in &class.accessors {
        let helper = instance_helper_name(class, &accessor.field);
        let symbol = types::wrap_method(&class.name, &accessor.field);
        let _ = writeln!(out, "{helper} <- function(self) .Call({symbol}, self)");
        let _ = writeln!(
            arms,
            "    {} = function() {helper}(x),",
            types::r_ident(&accessor.field),
        );
    }
    for method in &class.methods {
        let helper = instance_helper_name(class, &method.name);
        let symbol = types::wrap_method(&class.name, &method.name);
        let params: Vec<String> = method
            .params
            .iter()
            .map(|p| types::r_ident(&p.name))
            .collect();
        let call_args: Vec<String> = std::iter::once("self".to_string())
            .chain(params.clone())
            .collect();
        let _ = writeln!(
            out,
            "{helper} <- function({}) .Call({symbol}, {})",
            call_args.join(", "),
            call_args.join(", "),
        );
        let fwd_args: Vec<String> = std::iter::once("x".to_string())
            .chain(params.clone())
            .collect();
        let _ = writeln!(
            arms,
            "    {} = function({}) {helper}({}),",
            types::r_ident(&method.name),
            params.join(", "),
            fwd_args.join(", "),
        );
    }
    if !arms.is_empty() {
        let _ = writeln!(
            out,
            "\n\"$.{}\" <- function(x, name) {{\n  switch(name,\n{arms}    stop(\"unknown method \", name)\n  )\n}}",
            class.name,
        );
    }
    out.push('\n');
    out
}

/// `src/entrypoint.c`: forwards routine registration to Rust, since R looks
/// up the init routine by the shared object's own name, not the crate's.
pub(crate) fn entrypoint_c(lib: &str) -> String {
    format!(
        "{}//\n\
         // We need to forward routine registration from C to Rust\n\
         // to avoid the linker removing the static library.\n\n\
         void R_init_{lib}_extendr(void *dll);\n\
         void register_extendr_panic_hook(void);\n\n\
         void R_init_{lib}(void *dll) {{\n    \
         register_extendr_panic_hook();\n    \
         R_init_{lib}_extendr(dll);\n}}\n",
        crate::GENERATED_RS,
    )
}

pub(crate) fn makevars(lib: &str) -> String {
    format!(
        "{GENERATED_R}\nTARGET_DIR = ./rust/target\nLIBDIR = $(TARGET_DIR)/release\n\
         STATLIB = $(LIBDIR)/lib{lib}.a\nPKG_LIBS = -L$(LIBDIR) -l{lib}\n\n\
         all: $(SHLIB)\n\n\
         $(SHLIB): $(STATLIB)\n\n\
         $(STATLIB):\n\tcargo build --lib --release --manifest-path=./rust/Cargo.toml \
         --target-dir=\"$(TARGET_DIR)\"\n\n\
         clean:\n\trm -Rf $(SHLIB) $(OBJECTS) \"$(TARGET_DIR)\"\n",
    )
}

/// R's own build for Windows always targets the 64-bit mingw toolchain: R
/// dropped 32-bit Windows support in 4.2, so there is only the one triple to
/// build for.
pub(crate) fn makevars_win(lib: &str) -> String {
    format!(
        "{GENERATED_R}\nTARGET = x86_64-pc-windows-gnu\n\nTARGET_DIR = ./rust/target\n\
         LIBDIR = $(TARGET_DIR)/$(TARGET)/release\nSTATLIB = $(LIBDIR)/lib{lib}.a\n\
         PKG_LIBS = -L$(LIBDIR) -l{lib} -lws2_32 -ladvapi32 -luserenv -lbcrypt -lntdll\n\n\
         all: $(SHLIB)\n\n\
         $(SHLIB): $(STATLIB)\n\n\
         $(STATLIB):\n\tcargo build --lib --release --target=$(TARGET) \
         --manifest-path=./rust/Cargo.toml --target-dir=\"$(TARGET_DIR)\"\n\n\
         clean:\n\trm -Rf $(SHLIB) $(OBJECTS) \"$(TARGET_DIR)\"\n",
    )
}

/// `src/rust/Cargo.toml`. The bound crate sits two directories above where
/// every other backend's manifest would put it — `<out>/src/rust/` instead
/// of `<out>/` — so `crate_path` needs two extra steps up to reach it.
pub(crate) fn cargo_toml(opts: &BindOptions, lib: &str) -> String {
    let version = opts.backend_version.as_deref().unwrap_or(DEFAULT_VERSION);
    format!(
        "{GENERATED_TOML}\n[package]\nname = \"{lib}-extendr\"\nversion = \"{}\"\n\
         edition = \"2024\"\npublish = false\n\n\
         [workspace]\n\n\
         [lib]\nname = \"{lib}\"\ncrate-type = [\"staticlib\"]\n\n\
         [dependencies]\n{} = {{ path = \"../../{}\" }}\nextendr-api = \"{version}\"\n\n\
         [profile.release]\nlto = true\ncodegen-units = 1\n",
        opts.version, opts.crate_package, opts.crate_path,
    )
}

pub(crate) fn readme(plan: &BindingPlan, opts: &BindOptions) -> String {
    let mut out = format!("# {}\n\n", opts.package);
    if let Some(description) = &opts.description {
        let _ = writeln!(out, "{description}\n");
    }
    let _ = write!(
        out,
        "R bindings for the `{}` crate, generated by \
         [soothfast](https://github.com/Verdenroz/soothfast). Do not edit by hand: \
         run `cargo soothfast bind gen` instead.\n\n\
         ## Install\n\n\
         ```r\ninstall.packages(\"{}\", repos = NULL, type = \"source\")\n```\n\n\
         Building it needs a Rust toolchain (`cargo`, `rustc`) on `PATH`; the R \
         package itself has no other system dependency.\n\n\
         `src/rust/Cargo.toml` depends on the bound crate outside this tree, so \
         a `R CMD build` tarball of this package alone cannot be installed; \
         vendor the bound crate under `src/rust` first to make one distributable.\n\n\
         `DESCRIPTION` carries no `License` field: soothfast has no license to \
         put there, so add one before publishing.\n\n\
         A borrowed numeric or raw vector parameter (`&[f64]`, `&[u8]`) reads R's \
         own vector without copying it.\n",
        opts.crate_name, opts.package,
    );

    if !plan.classes.iter().any(|c| !c.is_plain_enum()) {
        return finish_readme(out, plan);
    }
    out.push_str("\n## Types\n\n");
    for class in plan.classes.iter().filter(|c| !c.is_plain_enum()) {
        let _ = writeln!(out, "### `{}`\n", class.name);
        if let Some(doc) = &class.doc {
            let _ = writeln!(out, "{doc}\n");
        }
    }
    finish_readme(out, plan)
}

fn finish_readme(mut out: String, plan: &BindingPlan) -> String {
    if !plan.functions.is_empty() {
        out.push_str("## Functions\n\n");
        for function in &plan.functions {
            let _ = writeln!(out, "- `{}`", types::r_ident(&function.name));
        }
        out.push('\n');
    }
    if !plan.gaps.is_empty() {
        out.push_str("## Not bound\n\n");
        for gap in &plan.gaps {
            let _ = writeln!(out, "- {}", gap.explain());
        }
    }
    out
}
