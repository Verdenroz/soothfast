//! Every shape the plan can produce, checked against every backend: for each
//! (shape, kind) cell, the shape is either reported as a `Gap` or the
//! emitted Rust glue compiles against `tests/shapes_crate`. Host-language
//! text (Go, Java, Kotlin, R, Ruby, C++, Lua, C#) is checked by the existing
//! smoke tests; here only the Rust glue every backend renders has to
//! compile.
//!
//! Slow and network-reaching (crates.io for pyo3/napi/wasm-bindgen/jni/
//! extendr-api/magnus): run with
//! `cargo test -p soothfast-bind --test shape_matrix -- --ignored`.

#[path = "support/mod.rs"]
mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use soothfast_bind::model::{
    ExportedFn, ExportedType, Field, Ownership, Param, Receiver, Surface, Ty, TypeKind, Variant,
    VariantFields,
};
use soothfast_bind::{BindKind, BindOptions};

/// `package`/`module` deliberately differ from the `shapes` crate name: some
/// backends (wasm, napi) name their own glue package after `package` with no
/// suffix, which would collide with `shapes` itself in the lockfile if the
/// two matched.
fn opts() -> BindOptions {
    BindOptions {
        package: "shapesbind".into(),
        module: "shapesbind".into(),
        version: "0.1.0".into(),
        crate_name: "shapes".into(),
        crate_package: "shapes".into(),
        ..BindOptions::default()
    }
}

fn param(name: &str, ty: Ty) -> Param {
    Param {
        name: name.into(),
        ty,
        ownership: Ownership::Owned,
        inner_ownership: Ownership::Owned,
    }
}

fn param_borrowed(name: &str, ty: Ty) -> Param {
    Param {
        ownership: Ownership::Borrowed,
        ..param(name, ty)
    }
}

fn param_mut(name: &str, ty: Ty) -> Param {
    Param {
        ownership: Ownership::BorrowedMut,
        ..param(name, ty)
    }
}

fn free(id: &str, params: Vec<Param>, ret: Ty) -> ExportedFn {
    ExportedFn {
        id: id.into(),
        rust_path: id.into(),
        name: id.rsplit("::").next().expect("has a name").into(),
        owner: None,
        receiver: Receiver::None,
        params,
        ret,
        throws: None,
        is_async: false,
        constructor: false,
        doc: None,
        skip: Vec::new(),
    }
}

fn ctor(id: &str, owner: &str, params: Vec<Param>, ret: Ty) -> ExportedFn {
    ExportedFn {
        owner: Some(owner.into()),
        constructor: true,
        ..free(id, params, ret)
    }
}

fn method(id: &str, owner: &str, params: Vec<Param>, ret: Ty) -> ExportedFn {
    ExportedFn {
        owner: Some(owner.into()),
        receiver: Receiver::Shared,
        ..free(id, params, ret)
    }
}

fn ty(name: &str, kind: TypeKind) -> ExportedType {
    ExportedType {
        id: format!("shapes::{name}"),
        rust_path: format!("shapes::{name}"),
        name: name.into(),
        kind,
        send: true,
        sync: true,
        doc: None,
        skip: Vec::new(),
    }
}

fn field(name: &str, ty: Ty) -> Field {
    Field {
        name: name.into(),
        ty,
        public: true,
        doc: None,
    }
}

fn variant(name: &str) -> Variant {
    Variant {
        name: name.into(),
        fields: VariantFields::Unit,
        doc: None,
    }
}

/// One shape per bug the branch-wide review verified, named by the exact
/// [`soothfast_bind::gap::Gap::at`] string a gap for it carries, or the
/// [`soothfast_bind::plan::Function::rust_path`]/accessor field a backend
/// binds it under when it is not gapped.
const SHAPES: &[&str] = &[
    "shapes::Handle::new",
    "shapes::Bag::new",
    "shapes::Wrap::new",
    "shapes::Flag::describe",
    "shapes::mutate_handle",
    "shapes::option_handle_param",
    "shapes::handle_list_ret",
    "shapes::option_flag_ret",
    "shapes::option_flag_param",
    "shapes::flag_ref_param",
    "shapes::flag_list_ret",
    "shapes::usize_list_probe",
    "shapes::option_list_probe",
    "shapes::bool_list_probe",
    "shapes::option_f64_param",
    "shapes::digest_mut",
    "Wrap.inner",
];

fn surface() -> Surface {
    let handle = ty("Handle", TypeKind::Struct(vec![field("value", Ty::I64)]));
    let flag = ty("Flag", TypeKind::Enum(vec![variant("A"), variant("B")]));
    let bag = ty(
        "Bag",
        TypeKind::Struct(vec![field("values", Ty::List(Box::new(Ty::F64)))]),
    );
    let wrap = ty(
        "Wrap",
        TypeKind::Struct(vec![field(
            "inner",
            Ty::Optional(Box::new(Ty::Class("Handle".into()))),
        )]),
    );

    let handle_ty = Ty::Class("Handle".into());
    let flag_ty = Ty::Class("Flag".into());
    let fns = vec![
        ctor(
            "shapes::Handle::new",
            "Handle",
            vec![param("value", Ty::I64)],
            handle_ty.clone(),
        ),
        ctor(
            "shapes::Bag::new",
            "Bag",
            Vec::new(),
            Ty::Class("Bag".into()),
        ),
        ctor(
            "shapes::Wrap::new",
            "Wrap",
            Vec::new(),
            Ty::Class("Wrap".into()),
        ),
        method("shapes::Flag::describe", "Flag", Vec::new(), Ty::Bool),
        free(
            "shapes::mutate_handle",
            vec![param_mut("h", handle_ty.clone())],
            Ty::Bool,
        ),
        free(
            "shapes::option_handle_param",
            vec![param("h", Ty::Optional(Box::new(handle_ty.clone())))],
            Ty::Bool,
        ),
        free(
            "shapes::handle_list_ret",
            Vec::new(),
            Ty::List(Box::new(handle_ty.clone())),
        ),
        free(
            "shapes::option_flag_ret",
            Vec::new(),
            Ty::Optional(Box::new(flag_ty.clone())),
        ),
        free(
            "shapes::option_flag_param",
            vec![param("flag", Ty::Optional(Box::new(flag_ty.clone())))],
            Ty::Bool,
        ),
        free(
            "shapes::flag_ref_param",
            vec![param_borrowed("flag", flag_ty.clone())],
            Ty::Bool,
        ),
        free(
            "shapes::flag_list_ret",
            Vec::new(),
            Ty::List(Box::new(flag_ty.clone())),
        ),
        free(
            "shapes::usize_list_probe",
            vec![param("v", Ty::List(Box::new(Ty::USize)))],
            Ty::List(Box::new(Ty::USize)),
        ),
        free(
            "shapes::option_list_probe",
            vec![param(
                "v",
                Ty::Optional(Box::new(Ty::List(Box::new(Ty::F64)))),
            )],
            Ty::Optional(Box::new(Ty::List(Box::new(Ty::F64)))),
        ),
        free(
            "shapes::bool_list_probe",
            vec![param("v", Ty::List(Box::new(Ty::Bool)))],
            Ty::List(Box::new(Ty::Bool)),
        ),
        free(
            "shapes::option_f64_param",
            vec![param("v", Ty::Optional(Box::new(Ty::F64)))],
            Ty::Bool,
        ),
        free(
            "shapes::digest_mut",
            vec![param_mut("buf", Ty::Bytes)],
            Ty::Bool,
        ),
    ];

    Surface {
        fns,
        types: vec![handle, flag, bag, wrap],
    }
}

/// Whether a shape is bound anywhere in the plan: as a callable, or as a
/// class accessor named `Class.field`.
fn item_bound(plan: &soothfast_bind::plan::BindingPlan, id: &str) -> bool {
    plan.functions().any(|f| f.rust_path == id)
        || plan.classes.iter().any(|c| {
            c.accessors
                .iter()
                .any(|a| format!("{}.{}", c.name, a.field) == id)
        })
}

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).into()
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("makes a directory");
    for entry in std::fs::read_dir(src).expect("reads a directory") {
        let entry = entry.expect("reads a directory entry");
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copies a file");
        }
    }
}

/// The prefix under which the emitted `Cargo.toml` and its sibling
/// `src/lib.rs` sit: the crate root for every backend, and a subdirectory of
/// the whole package for the ones that nest it (`ext/<module>/` for Ruby,
/// `src/rust/` for R).
fn crate_root(files: &BTreeMap<String, String>) -> Option<String> {
    files.keys().find_map(|k| {
        let prefix = k.strip_suffix("Cargo.toml")?;
        files
            .contains_key(&format!("{prefix}src/lib.rs"))
            .then(|| prefix.to_string())
    })
}

/// Stage the whole package a `BindFileSet` carries, nested exactly as
/// emitted, beside a copy of `tests/shapes_crate`: a wrapper crate's own
/// `Cargo.toml` (R nests one three directories under `glue/`, Ruby two)
/// hardcodes a relative `path` back to it at that exact depth. Returns the
/// staging root and the directory holding the crate to check.
fn stage(files: &BTreeMap<String, String>, tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "soothfast-shapes-{tag}-check-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    copy_dir(&manifest_dir().join("tests/shapes_crate"), &root);
    let glue = root.join("glue");
    let prefix = crate_root(files).expect("a Cargo.toml with a sibling src/lib.rs");
    for (rel, content) in files {
        let target = glue.join(rel);
        std::fs::create_dir_all(target.parent().expect("has a parent")).expect("makes dirs");
        std::fs::write(&target, content).expect("writes");
    }
    (root, glue.join(prefix))
}

/// `cargo check` the Rust glue for one kind, returning its stderr on
/// failure.
fn check(files: &BTreeMap<String, String>, tag: &str, extra: &[&str]) -> Result<(), String> {
    let (root, crate_dir) = stage(files, tag);
    let output = Command::new("cargo")
        .arg("check")
        .args(extra)
        .current_dir(&crate_dir)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .expect("runs cargo check");
    let _ = std::fs::remove_dir_all(&root);
    match output.status.success() {
        true => Ok(()),
        false => Err(String::from_utf8_lossy(&output.stderr).into_owned()),
    }
}

/// A kind whose Rust glue needs a host toolchain present at `cargo check`
/// time (pyo3 and extendr both probe an interpreter from their `build.rs`;
/// wasm-bindgen needs its target installed), and the check that toolchain is
/// there. Every other kind is plain Rust against crates.io dependencies.
fn toolchain_gate(kind: BindKind) -> Option<(bool, &'static str, &'static str)> {
    match kind {
        BindKind::Python => Some((
            Command::new("python3").arg("--version").output().is_ok(),
            "python3",
            "install Python 3",
        )),
        BindKind::Wasm => Some((
            Command::new("rustup")
                .args(["target", "list", "--installed"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains("wasm32-unknown-unknown"))
                .unwrap_or(false),
            "wasm32-unknown-unknown",
            "rustup target add wasm32-unknown-unknown",
        )),
        BindKind::R => Some((
            Command::new("R").arg("--version").output().is_ok(),
            "R",
            "install R",
        )),
        BindKind::Ruby => Some((
            Command::new("ruby").arg("--version").output().is_ok(),
            "ruby",
            "install Ruby",
        )),
        _ => None,
    }
}

fn extra_check_args(kind: BindKind) -> &'static [&'static str] {
    match kind {
        BindKind::Wasm => &["--target", "wasm32-unknown-unknown"],
        _ => &[],
    }
}

#[test]
#[ignore = "compiles a real crate per backend against crates.io dependencies"]
fn every_shape_is_gapped_or_compiles() {
    let surface = surface();
    let opts = opts();
    let mut failing: Vec<String> = Vec::new();

    for &kind in BindKind::ALL {
        if let Some((available, tool, hint)) = toolchain_gate(kind)
            && !support::require_toolchain(available, tool, hint)
        {
            continue;
        }

        let plan = soothfast_bind::plan::lower(&surface, Vec::new(), &opts, kind).expect("lowers");
        let gapped: Vec<&str> = plan.gaps.iter().map(|g| g.at()).collect();
        for &shape in SHAPES {
            let bound = item_bound(&plan, shape);
            let explained = gapped.contains(&shape);
            if !bound && !explained {
                failing.push(format!(
                    "{}: {shape} is neither bound nor reported as a gap",
                    kind.name()
                ));
            }
        }

        let emitted = kind.emit(&surface, Vec::new(), &opts).expect("emits");
        if let Err(stderr) = check(&emitted.files, kind.name(), extra_check_args(kind)) {
            let bound: Vec<&str> = SHAPES
                .iter()
                .copied()
                .filter(|s| item_bound(&plan, s))
                .collect();
            failing.push(format!(
                "{}: glue failed to compile (bound shapes: {}):\n{stderr}",
                kind.name(),
                bound.join(", "),
            ));
        }
    }

    assert!(failing.is_empty(), "\n{}", failing.join("\n\n"));
}
