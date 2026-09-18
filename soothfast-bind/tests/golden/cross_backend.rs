use soothfast_bind::BindKind;

use crate::fixture::{
    cpp_opts, csharp_opts, go_opts, java_opts, kotlin_opts, lua_opts, opts, plan_for, r_opts,
    ruby_opts,
};
use crate::{emit, emit_set_with};

#[test]
fn one_rust_surface_reaches_both_languages_as_the_same_class() {
    let python = emit(BindKind::Python)["src/lib.rs"].clone();
    let wasm = emit(BindKind::Wasm)["src/lib.rs"].clone();
    for glue in [&python, &wasm] {
        assert!(glue.contains("pub struct Counter(::acme::Counter);"));
        assert!(glue.contains("fn normalize("));
    }
    assert!(python.contains("#[pyclass(name = \"Counter\")]"));
    assert!(wasm.contains("#[wasm_bindgen]\npub struct Counter"));
}

/// A JS-facing name is a camelCased Rust one, and both backends camelCase
/// with the same reserved-word list, so the Rust names a plan carries are a
/// faithful stand-in for what each backend actually exports.
fn export_names(plan: &soothfast_bind::plan::BindingPlan) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    for class in &plan.classes {
        names.insert(format!("class:{}", class.name));
        if let Some(ctor) = &class.ctor {
            names.insert(format!("{}::{}", class.name, ctor.name));
        }
        for accessor in &class.accessors {
            names.insert(format!("{}.{}", class.name, accessor.field));
        }
        for method in &class.methods {
            names.insert(format!("{}::{}", class.name, method.name));
        }
        for s in &class.statics {
            names.insert(format!("{}::{}", class.name, s.name));
        }
        for variant in class.variants.iter().flatten() {
            names.insert(format!("{}::{}", class.name, variant.name));
        }
    }
    for f in &plan.functions {
        names.insert(format!("fn:{}", f.name));
    }
    names
}

/// The subset of `export_names` that is `async`: wasm binds one as a
/// `Promise`, Node gaps it entirely for lack of a runtime story.
fn async_export_names(
    plan: &soothfast_bind::plan::BindingPlan,
) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    for class in &plan.classes {
        for method in class
            .methods
            .iter()
            .chain(class.statics.iter())
            .filter(|f| f.is_async)
        {
            names.insert(format!("{}::{}", class.name, method.name));
        }
    }
    for f in plan.functions.iter().filter(|f| f.is_async) {
        names.insert(format!("fn:{}", f.name));
    }
    names
}

#[test]
fn node_exports_the_same_names_wasm_does_except_async() {
    let wasm_plan = plan_for(BindKind::Wasm);
    let node_plan = plan_for(BindKind::Node);
    let wasm = export_names(&wasm_plan);
    let node = export_names(&node_plan);
    assert!(
        node.is_subset(&wasm),
        "node exports a name wasm does not: {:?}",
        node.difference(&wasm).collect::<Vec<_>>()
    );
    let missing: std::collections::BTreeSet<String> = wasm.difference(&node).cloned().collect();
    assert_eq!(missing, async_export_names(&wasm_plan));
}

#[test]
fn an_optional_string_binds_across_the_c_family() {
    for kind in [
        BindKind::CAbi,
        BindKind::Go,
        BindKind::Java,
        BindKind::Kotlin,
        BindKind::Cpp,
        BindKind::Lua,
        BindKind::CSharp,
    ] {
        let opts = match kind {
            BindKind::Go => go_opts(),
            BindKind::Java => java_opts(),
            BindKind::Kotlin => kotlin_opts(),
            BindKind::Cpp => cpp_opts(),
            BindKind::Lua => lua_opts(),
            BindKind::CSharp => csharp_opts(),
            _ => opts(),
        };
        let set = emit_set_with(kind, &opts);
        assert!(
            !set.gaps.iter().any(|g| g.contains("describe")),
            "{kind:?} gaps describe: {:?}",
            set.gaps
        );
    }
}

#[test]
fn an_optional_scalar_is_a_gap_across_the_c_family() {
    for kind in [
        BindKind::CAbi,
        BindKind::Go,
        BindKind::Java,
        BindKind::Kotlin,
        BindKind::Cpp,
        BindKind::Lua,
        BindKind::CSharp,
    ] {
        let opts = match kind {
            BindKind::Go => go_opts(),
            BindKind::Java => java_opts(),
            BindKind::Kotlin => kotlin_opts(),
            BindKind::Cpp => cpp_opts(),
            BindKind::Lua => lua_opts(),
            BindKind::CSharp => csharp_opts(),
            _ => opts(),
        };
        let set = emit_set_with(kind, &opts);
        assert!(
            set.gaps.iter().any(|g| g.contains("maybe_ratio")),
            "{kind:?} does not gap maybe_ratio: {:?}",
            set.gaps
        );
        assert!(
            !set.files
                .iter()
                .any(|(name, f)| name != "README.md" && f.contains("maybe_ratio")),
            "{kind:?} emitted a symbol for maybe_ratio"
        );
    }
}

#[test]
fn an_optional_scalar_binds_natively_outside_the_c_family() {
    for kind in [
        BindKind::Python,
        BindKind::Wasm,
        BindKind::Node,
        BindKind::Ruby,
        BindKind::R,
    ] {
        let opts = match kind {
            BindKind::Ruby => ruby_opts(),
            BindKind::R => r_opts(),
            _ => opts(),
        };
        let set = emit_set_with(kind, &opts);
        assert!(
            !set.gaps.iter().any(|g| g.contains("maybe_ratio")),
            "{kind:?} gaps maybe_ratio: {:?}",
            set.gaps
        );
    }
}
