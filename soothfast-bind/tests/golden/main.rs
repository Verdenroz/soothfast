//! Emitted binding packages, pinned byte for byte.
//!
//! Regenerate with `UPDATE_GOLDENS=1 cargo test -p soothfast-bind`.

#[path = "../fixture/mod.rs"]
mod fixture;

mod c;
mod cpp;
mod cross_backend;
mod csharp;
mod go;
mod java;
mod kotlin;
mod lua;
mod node;
mod python;
mod r;
mod ruby;
mod wasm;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fixture::{opts, walk};
use soothfast_bind::{BindKind, BindOptions};

fn golden_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens")
        .join(name)
}

fn emit(kind: BindKind) -> BTreeMap<String, String> {
    emit_set(kind).files
}

fn emit_set(kind: BindKind) -> soothfast_bind::BindFileSet {
    emit_set_with(kind, &opts())
}

fn emit_set_with(kind: BindKind, opts: &BindOptions) -> soothfast_bind::BindFileSet {
    let (surface, gaps) = walk();
    kind.emit(&surface, gaps, opts).expect("emits")
}

fn walk_dir(dir: &Path, root: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let dotfile = matches!(name.as_str(), ".gitignore" | ".Rbuildignore");
        if (name.starts_with('.') && !dotfile) || name == "target" {
            continue;
        }
        if path.is_dir() {
            walk_dir(&path, root, out);
        } else if let Ok(content) = std::fs::read_to_string(&path) {
            let rel = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, content);
        }
    }
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("makes a directory");
    for entry in std::fs::read_dir(src).expect("reads a golden") {
        let entry = entry.expect("reads a directory entry");
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copies a golden file");
        }
    }
}

/// Stage a golden beside a copy of the fixture crate, the layout every
/// smoke test builds in: the glue crate's `Cargo.toml` depends on `acme` at
/// `path = ".."`.
fn stage_golden(lang: &str, tag: &str) -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = std::env::temp_dir().join(format!("soothfast-{tag}-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy_dir(&manifest.join("tests/fixture_crate"), &root);
    let glue = root.join("glue");
    copy_dir(&manifest.join("tests/goldens").join(lang), &glue);
    glue
}

fn check_goldens(kind: BindKind, name: &str) {
    check_goldens_with(kind, name, &opts());
}

fn check_goldens_with(kind: BindKind, name: &str, opts: &BindOptions) {
    let files = emit_set_with(kind, opts).files;
    let dir = golden_dir(name);

    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        let _ = std::fs::remove_dir_all(&dir);
        for (rel, content) in &files {
            let target = dir.join(rel);
            std::fs::create_dir_all(target.parent().expect("has a parent")).expect("makes dirs");
            std::fs::write(&target, content).expect("writes");
        }
        return;
    }

    let mut expected = BTreeMap::new();
    walk_dir(&dir, &dir, &mut expected);
    assert!(
        !expected.is_empty(),
        "no {name} goldens found; run UPDATE_GOLDENS=1 cargo test -p soothfast-bind"
    );
    let got: Vec<&String> = files.keys().collect();
    let want: Vec<&String> = expected.keys().collect();
    assert_eq!(got, want, "file set changed");
    for (rel, content) in &files {
        assert_eq!(content, &expected[rel], "content of {rel} changed");
    }
}
