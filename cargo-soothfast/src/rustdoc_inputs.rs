//! The files and variables a build read, taken from the dep-info rustc,
//! rustdoc and cargo wrote for it and from build script output.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::invoke;
use crate::store::Inputs;

/// Everything the build read beyond the files of the package directories:
/// `dep_info` is what rustdoc wrote for the documented crate, and
/// `cargo_stdout` is cargo's `--message-format=json` output, which names the
/// dep-info of every workspace dependency and the build scripts that ran.
/// Registry sources are immutable and covered by the lockfile.
pub fn collect(root: &Path, dep_info: &Path, cargo_stdout: &[u8]) -> Inputs {
    let mut inputs = Inputs::default();
    if let Ok(text) = std::fs::read_to_string(dep_info) {
        add_dep_info(&mut inputs, root, &text);
    }
    walk(&mut inputs, root, cargo_stdout);
    inputs
}

/// The same for a build of the executable `exe`, whose own dep-info sits
/// beside it. An error naming the crates and the files tried when the
/// dep-info of any workspace crate cannot be read, since the build then read
/// files nobody knows of.
pub fn collect_build(root: &Path, exe: &Path, cargo_stdout: &[u8]) -> Result<Inputs, String> {
    let mut inputs = Inputs::default();
    let beside = dep_info_beside(exe);
    let text = std::fs::read_to_string(&beside)
        .map_err(|_| format!("{}: no dep-info at {}", exe.display(), beside.display()))?;
    add_dep_info(&mut inputs, root, &text);
    let unread = walk(&mut inputs, root, cargo_stdout);
    if unread.is_empty() {
        Ok(inputs)
    } else {
        Err(unread.join("; "))
    }
}

/// Add what each workspace artifact and build script of `cargo_stdout` read.
/// Names each artifact whose dep-info could not be read, with the files tried.
fn walk(inputs: &mut Inputs, root: &Path, cargo_stdout: &[u8]) -> Vec<String> {
    let mut unread = Vec::new();
    let mut package_dirs: BTreeMap<String, PathBuf> = BTreeMap::new();
    for msg in invoke::cargo_messages(cargo_stdout) {
        let Some(id) = msg["package_id"].as_str() else {
            continue;
        };
        let local = id.starts_with("path+");
        match msg["reason"].as_str() {
            Some("compiler-artifact") => {
                if let Some(dir) = msg["manifest_path"]
                    .as_str()
                    .and_then(|m| Path::new(m).parent())
                {
                    package_dirs.insert(id.to_string(), dir.to_path_buf());
                }
                if local && let Err(why) = add_artifact(inputs, root, &msg) {
                    unread.push(why);
                }
            }
            Some("build-script-executed") => {
                let dir = package_dirs.get(id).filter(|_| local);
                add_build_script(inputs, root, dir, &msg);
            }
            _ => {}
        }
    }
    unread
}

/// Reads the artifact's dep-info; an error says where it looked. An artifact
/// that names no file has none to read.
fn add_artifact(inputs: &mut Inputs, root: &Path, msg: &Value) -> Result<(), String> {
    let is_build_script = msg["target"]["kind"]
        .as_array()
        .is_some_and(|k| k.iter().any(|k| k == "custom-build"));
    if is_build_script && let Some(src) = msg["target"]["src_path"].as_str() {
        inputs.files.insert(relative(root, Path::new(src)));
    }
    let Some(first) = msg["filenames"][0].as_str() else {
        return Ok(());
    };
    let Some(text) = read_dep_info(Path::new(first)) else {
        let name = msg["target"]["name"].as_str().unwrap_or("?");
        let kind = msg["target"]["kind"][0].as_str().unwrap_or("?");
        return Err(format!(
            "{name} ({kind}): no dep-info beside {first} or its deps twin"
        ));
    };
    add_dep_info(inputs, root, &text);
    Ok(())
}

/// The dep-info of `artifact`, found beside it or beside the `deps/` file an
/// uplifted binary links to.
fn read_dep_info(artifact: &Path) -> Option<String> {
    let read = |a: &Path| std::fs::read_to_string(dep_info_beside(a)).ok();
    read(artifact).or_else(|| read(&deps_twin(artifact)?))
}

/// Cargo links a binary at `target/<profile>/name` to `deps/name-hash`, and a
/// build script at `build/pkg-hash/build-script-build` to
/// `build_script_build-hash` beside it. Its newer layout puts the file at
/// `build/pkg/hash/out/name` instead, with `name.d` beside. The file the
/// dep-info is named for is the one this finds.
#[cfg(unix)]
fn deps_twin(artifact: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;

    let parent = artifact.parent()?;
    let inode = std::fs::metadata(artifact).ok()?.ino();
    let same = |path: &Path| std::fs::metadata(path).is_ok_and(|m| m.ino() == inode);
    let named = parent
        .join("build")
        .read_dir()
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|pkg| {
            std::fs::read_dir(pkg.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .map(|hash| {
            hash.path().join("out").join(
                artifact
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .replace('-', "_"),
            )
        })
        .find(|candidate| same(candidate));
    named.or_else(|| {
        [parent.join("deps"), parent.to_path_buf()]
            .iter()
            .filter_map(|dir| std::fs::read_dir(dir).ok())
            .flatten()
            .flatten()
            .find(|e| {
                e.path() != artifact
                    && e.path().extension().is_none()
                    && e.metadata().is_ok_and(|m| m.ino() == inode)
            })
            .map(|e| e.path())
    })
}

#[cfg(not(unix))]
fn deps_twin(_: &Path) -> Option<PathBuf> {
    None
}

/// Cargo's `deps/libname-hash.rmeta` has its dep-info at `deps/name-hash.d`,
/// and an executable `deps/name-hash` at the same name.
fn dep_info_beside(artifact: &Path) -> PathBuf {
    let stem = artifact.file_stem().unwrap_or_default().to_string_lossy();
    let stem = match artifact.extension() {
        Some(_) => stem.strip_prefix("lib").unwrap_or(&stem),
        None => &stem,
    };
    artifact.with_file_name(format!("{stem}.d"))
}

/// A build script's `rerun-if-changed` paths (for workspace packages, which
/// are relative to the package) and every package's `rerun-if-env-changed`.
fn add_build_script(inputs: &mut Inputs, root: &Path, package_dir: Option<&PathBuf>, msg: &Value) {
    let Some(out_dir) = msg["out_dir"].as_str().map(Path::new) else {
        return;
    };
    let Ok(output) = std::fs::read_to_string(out_dir.with_file_name("output")) else {
        return;
    };
    for line in output.lines() {
        let directive = line
            .strip_prefix("cargo::")
            .or_else(|| line.strip_prefix("cargo:"));
        match directive.and_then(|d| d.split_once('=')) {
            Some(("rerun-if-changed", path)) => {
                if let Some(dir) = package_dir {
                    inputs.files.insert(relative(root, &dir.join(path)));
                }
            }
            Some(("rerun-if-env-changed", name)) => {
                inputs.env.insert(name.to_string());
            }
            _ => {}
        }
    }
}

/// The files on the first line of a Makefile-style dep-info (spaces in a
/// path escaped with a backslash) and the variables on its `env-dep` lines.
fn add_dep_info(inputs: &mut Inputs, root: &Path, text: &str) {
    if let Some(first) = text.lines().next()
        && let Some((_, deps)) = first.split_once(": ")
    {
        let mut path = String::new();
        let mut chars = deps.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' if chars.peek() == Some(&' ') => {
                    path.push(' ');
                    chars.next();
                }
                ' ' if !path.is_empty() => {
                    inputs.files.insert(relative(root, Path::new(&path)));
                    path.clear();
                }
                ' ' => {}
                c => path.push(c),
            }
        }
        if !path.is_empty() {
            inputs.files.insert(relative(root, Path::new(&path)));
        }
    }
    for line in text.lines() {
        if let Some(var) = line.strip_prefix("# env-dep:") {
            let name = var.split('=').next().unwrap_or(var);
            if !name.starts_with("CARGO_PKG_") {
                inputs.env.insert(name.to_string());
            }
        }
    }
}

/// `path` relative to `root` when it lies under it, so the same input reads
/// the same in another checkout.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("soothfast-inputs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn dep_info_lists_files_with_escaped_spaces_and_tracked_variables() {
        let root = Path::new("/w");
        let text = "/w/t/doc/x.json: src/lib.rs /w/docs/a\\ b.md ../README.md\n\
                    src/lib.rs:\n\
                    # env-dep:CARGO_PKG_VERSION=1.0.0\n\
                    # env-dep:API_BASE=https://example.test\n";
        let mut inputs = Inputs::default();
        add_dep_info(&mut inputs, root, text);
        let files: Vec<_> = inputs.files.iter().map(String::as_str).collect();
        assert_eq!(files, ["../README.md", "docs/a b.md", "src/lib.rs"]);
        let env: Vec<_> = inputs.env.iter().map(String::as_str).collect();
        assert_eq!(env, ["API_BASE"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_linked_out_of_deps_finds_its_dep_info() {
        let root = temp("twin");
        let deps = root.join("target/debug/deps");
        std::fs::create_dir_all(&deps).unwrap();
        std::fs::write(deps.join("app-1a2b"), "bin").unwrap();
        std::fs::write(deps.join("app-1a2b.d"), "x: src/main.rs\n").unwrap();
        std::fs::write(deps.join("other-3c4d"), "bin").unwrap();
        std::fs::hard_link(deps.join("app-1a2b"), root.join("target/debug/app")).unwrap();
        let text = read_dep_info(&root.join("target/debug/app")).unwrap();
        assert_eq!(text, "x: src/main.rs\n");
        std::fs::write(root.join("target/debug/lone"), "bin").unwrap();
        assert_eq!(read_dep_info(&root.join("target/debug/lone")), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_in_the_per_package_build_layout_finds_its_dep_info() {
        let root = temp("layout");
        let out = root.join("target/debug/build/app/9f8e/out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("app_server"), "bin").unwrap();
        std::fs::write(out.join("app_server.d"), "x: src/main.rs\n").unwrap();
        std::fs::hard_link(out.join("app_server"), root.join("target/debug/app-server")).unwrap();
        let text = read_dep_info(&root.join("target/debug/app-server")).unwrap();
        assert_eq!(text, "x: src/main.rs\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_artifact_without_dep_info_is_named_with_the_file_tried() {
        let root = temp("unread");
        let stdout =
            json!({"reason": "compiler-artifact", "package_id": "path+file:///w/app#app@0.1.0",
            "manifest_path": root.join("app/Cargo.toml"),
            "target": {"name": "app", "kind": ["bin"]},
            "filenames": [root.join("target/debug/app")]})
            .to_string();
        let mut inputs = Inputs::default();
        let unread = walk(&mut inputs, &root, stdout.as_bytes());
        assert_eq!(unread.len(), 1);
        assert!(unread[0].starts_with("app (bin): no dep-info beside "));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cargo_output_names_dependency_dep_info_and_build_script_inputs() {
        let root = temp("cargo");
        let deps = root.join("target/debug/deps");
        std::fs::create_dir_all(&deps).unwrap();
        std::fs::write(
            deps.join("dep-1a2b.d"),
            format!(
                "{0}/target/debug/deps/dep-1a2b.rmeta: dep/src/lib.rs ../shared/types.rs\n",
                root.display()
            ),
        )
        .unwrap();
        let out_dir = root.join("target/debug/build/dep-9/out");
        std::fs::create_dir_all(&out_dir).unwrap();
        std::fs::write(
            out_dir.with_file_name("output"),
            "cargo:rerun-if-changed=schema/api.json\ncargo::rerun-if-env-changed=DEP_TOKEN\ncargo:rustc-cfg=x\n",
        )
        .unwrap();
        let registry_out = root.join("target/debug/build/reg-1/out");
        std::fs::create_dir_all(&registry_out).unwrap();
        std::fs::write(
            registry_out.with_file_name("output"),
            "cargo:rerun-if-changed=vendor.c\ncargo:rerun-if-env-changed=REG_TOKEN\n",
        )
        .unwrap();

        let id = "path+file:///w/dep#dep@0.1.0";
        let stdout = [
            json!({"reason": "compiler-artifact", "package_id": id,
                   "manifest_path": root.join("dep/Cargo.toml"),
                   "target": {"kind": ["lib"]},
                   "filenames": [deps.join("libdep-1a2b.rmeta")]}),
            json!({"reason": "compiler-artifact", "package_id": id,
                   "manifest_path": root.join("dep/Cargo.toml"),
                   "target": {"kind": ["custom-build"], "src_path": root.join("dep/build.rs")},
                   "filenames": []}),
            json!({"reason": "build-script-executed", "package_id": id,
                   "out_dir": out_dir}),
            json!({"reason": "build-script-executed",
                   "package_id": "registry+https://github.com/rust-lang/crates.io-index#reg@1.0.0",
                   "out_dir": registry_out}),
            json!({"reason": "build-finished", "success": true}),
        ]
        .iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join("\n");

        let inputs = collect(&root, &root.join("missing.d"), stdout.as_bytes());
        let files: Vec<_> = inputs.files.iter().map(String::as_str).collect();
        assert_eq!(
            files,
            [
                "../shared/types.rs",
                "dep/build.rs",
                "dep/schema/api.json",
                "dep/src/lib.rs",
            ]
        );
        let env: Vec<_> = inputs.env.iter().map(String::as_str).collect();
        assert_eq!(env, ["DEP_TOKEN", "REG_TOKEN"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
