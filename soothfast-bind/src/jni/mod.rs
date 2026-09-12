//! Java bindings, over JNI.
//!
//! The first garbage-collected target. Every native method is `static`,
//! taking a handle's pointer as a plain `long` rather than reading it back
//! off the Java object, so the Rust side never has to look up a field by
//! name and a handle's actual type-safety comes from Java's own type
//! checker choosing which object's pointer gets passed where.

mod glue;
mod java;
mod package;
mod types;

use crate::naming;
use crate::plan::BindingPlan;
use crate::{BindFileSet, BindOptions};

/// The jni crate release the generated glue builds against, unless the
/// `[[bind]]` entry pins another.
pub(crate) const DEFAULT_VERSION: &str = "0.21";

const KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "try",
    "void",
    "volatile",
    "while",
    "true",
    "false",
    "null",
];

/// A Rust name as the Java identifier it is exported under.
pub(crate) fn java_ident(name: &str) -> String {
    naming::escape(&naming::camel(name), KEYWORDS)
}

/// The `<os>-<arch>` a JNI loader stages a target's cdylib under, matching
/// how `Natives.java` names the same directory from `os.name`/`os.arch` at
/// runtime. Neither side reads npm's os/cpu naming (`darwin`, `win32`,
/// `x64`); this is the one naming both sides own.
pub(crate) fn native_dir_for_triple(triple: &str) -> String {
    let arch = if triple.starts_with("aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let os = if triple.contains("apple-darwin") {
        "macos"
    } else if triple.contains("windows") {
        "windows"
    } else {
        "linux"
    };
    format!("{os}-{arch}")
}

/// Emit a complete Java binding package: the glue crate plus the Java
/// sources it hands the JVM.
pub(crate) fn emit(plan: &BindingPlan, opts: &BindOptions) -> Result<BindFileSet, String> {
    let mut out = BindFileSet {
        notes: notes(plan),
        ..BindFileSet::default()
    };
    let files = &mut out.files;
    files.insert("Cargo.toml".into(), package::cargo_toml(opts));
    files.insert("README.md".into(), package::readme(plan, opts));
    files.insert(".gitignore".into(), "target/\n".into());
    files.insert("src/lib.rs".into(), glue::render(plan, opts));
    for (path, content) in java::render(plan, opts) {
        files.insert(path, content);
    }
    Ok(out)
}

/// Shapes Java cannot take as precisely as the Rust states them, plus what a
/// pinned buffer forecloses.
fn notes(plan: &BindingPlan) -> Vec<String> {
    let mut out: Vec<String> = plan
        .classes
        .iter()
        .filter(|c| c.variants.is_some() && !c.is_plain_enum())
        .map(|c| {
            format!(
                "{}: an enum carrying data binds as an opaque handle; its \
                 variants are not visible from Java",
                c.name
            )
        })
        .collect();
    out.extend(crate::plan::transfer_notes(plan));
    out
}

#[cfg(test)]
mod tests {
    use super::native_dir_for_triple;

    #[test]
    fn every_mainstream_triple_maps_to_javas_own_os_and_arch_names() {
        assert_eq!(
            native_dir_for_triple("x86_64-unknown-linux-gnu"),
            "linux-x86_64"
        );
        assert_eq!(
            native_dir_for_triple("aarch64-unknown-linux-gnu"),
            "linux-aarch64"
        );
        assert_eq!(native_dir_for_triple("x86_64-apple-darwin"), "macos-x86_64");
        assert_eq!(
            native_dir_for_triple("aarch64-apple-darwin"),
            "macos-aarch64"
        );
        assert_eq!(
            native_dir_for_triple("x86_64-pc-windows-msvc"),
            "windows-x86_64"
        );
    }
}
