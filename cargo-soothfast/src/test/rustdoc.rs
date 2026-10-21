//! The `RUSTDOC` wrapper `--no-doc` installs for the run step.
//!
//! Selecting targets cannot drop doc tests without also dropping units the
//! build made, so the run step keeps the build's selection and the wrapper
//! skips the doc tests instead. It is reached through a link named [`LINK`]
//! beside the plan: test processes inherit `RUSTDOC`, so dispatch keys on
//! the program name, and anything outside the plan passes through.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::shim::{DocSkip, Plan};

/// The wrapper's program name.
pub const LINK: &str = "soothfast-rustdoc";
/// The plan file, beside the link.
pub const PLAN_FILE: &str = "plan.json";

/// Whether `argv0` names the wrapper.
pub fn invoked_as(argv0: &str) -> bool {
    Path::new(argv0).file_stem().is_some_and(|s| s == LINK)
}

/// Link the running executable into `dir` as the wrapper, copying it when
/// the filesystem refuses a hard link.
pub fn install(dir: &Path) -> io::Result<PathBuf> {
    let link = dir.join(format!("{LINK}{}", std::env::consts::EXE_SUFFIX));
    let exe = std::env::current_exe()?;
    let _ = std::fs::remove_file(&link);
    if std::fs::hard_link(&exe, &link).is_err() {
        std::fs::copy(&exe, &link)?;
    }
    Ok(link)
}

/// Entry point when invoked as the wrapper.
pub fn run(argv0: &str, args: &[String]) -> i32 {
    let plan = Path::new(argv0)
        .parent()
        .and_then(|d| Plan::read(&d.join(PLAN_FILE)).ok());
    let skip = plan.as_ref().and_then(|p| p.doc_skip.as_ref());
    let cwd = std::env::current_dir().unwrap_or_default();
    if let Some(name) = skip.and_then(|s| skipped(s, args, &cwd)) {
        eprintln!("test: skip doc tests for {name} (--no-doc)");
        return 0;
    }
    let real = skip
        .and_then(|s| s.original.clone())
        .unwrap_or_else(|| "rustdoc".into());
    match Command::new(&real).args(args).status() {
        Ok(status) => status.code().unwrap_or(101),
        Err(e) => {
            eprintln!("soothfast: cannot run {real}: {e}");
            101
        }
    }
}

/// The crate whose doc tests `args` would run, when `skip` covers it. An
/// argument file hides the arguments, so it passes through.
fn skipped<'a>(skip: &DocSkip, args: &'a [String], cwd: &Path) -> Option<&'a str> {
    if !args.iter().any(|a| a == "--test") || args.iter().any(|a| a.starts_with('@')) {
        return None;
    }
    let name = args
        .iter()
        .position(|a| a == "--crate-name")
        .and_then(|i| args.get(i + 1))?;
    let planned = skip.crates.iter().any(|c| c == name);
    let in_workspace = args.iter().filter(|a| a.ends_with(".rs")).any(|a| {
        let path = cwd.join(a);
        path.canonicalize().unwrap_or(path).starts_with(&skip.root)
    });
    (planned && in_workspace).then_some(name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skip() -> DocSkip {
        DocSkip {
            original: None,
            crates: vec!["mylib".into()],
            root: PathBuf::from("/ws"),
        }
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn doctest(name: &str, src: &str) -> Vec<String> {
        args(&[
            "--edition=2024",
            "--crate-type",
            "lib",
            "--crate-name",
            name,
            "--test",
            src,
        ])
    }

    #[test]
    fn a_planned_crate_under_the_workspace_is_skipped() {
        let a = doctest("mylib", "mylib/src/lib.rs");
        assert_eq!(skipped(&skip(), &a, Path::new("/ws")), Some("mylib"));
    }

    #[test]
    fn a_crate_outside_the_plan_passes_through() {
        let a = doctest("generated", "generated/src/lib.rs");
        assert_eq!(skipped(&skip(), &a, Path::new("/ws")), None);
    }

    #[test]
    fn a_source_outside_the_workspace_passes_through() {
        let a = doctest("mylib", "/tmp/gen/mylib/src/lib.rs");
        assert_eq!(skipped(&skip(), &a, Path::new("/ws")), None);
    }

    #[test]
    fn an_invocation_without_test_passes_through() {
        let a = args(&["--crate-name", "mylib", "mylib/src/lib.rs"]);
        assert_eq!(skipped(&skip(), &a, Path::new("/ws")), None);
    }

    #[test]
    fn an_argument_file_passes_through() {
        let mut a = doctest("mylib", "mylib/src/lib.rs");
        a.push("@/tmp/args".into());
        assert_eq!(skipped(&skip(), &a, Path::new("/ws")), None);
    }

    #[test]
    fn only_the_link_name_dispatches_to_the_wrapper() {
        assert!(invoked_as("/ws/.soothfast/test-run/1/soothfast-rustdoc"));
        assert!(invoked_as("soothfast-rustdoc.exe"));
        assert!(!invoked_as("/usr/bin/cargo-soothfast"));
    }

    #[cfg(unix)]
    #[test]
    fn the_wrapper_skips_planned_doc_tests_and_runs_the_original_otherwise() {
        let dir = std::env::temp_dir().join(format!("soothfast-rustdoc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("mylib/src")).unwrap();
        let root = dir.canonicalize().unwrap();
        let plan = Plan {
            doc_skip: Some(DocSkip {
                original: Some("/bin/sh".into()),
                crates: vec!["mylib".into()],
                root: root.clone(),
            }),
            ..Plan::default()
        };
        plan.write(&root.join(PLAN_FILE)).unwrap();
        let argv0 = root.join(LINK).to_string_lossy().into_owned();
        let src = root.join("mylib/src/lib.rs").to_string_lossy().into_owned();
        std::fs::write(&src, "").unwrap();
        assert_eq!(run(&argv0, &doctest("mylib", &src)), 0);
        assert_eq!(run(&argv0, &args(&["-c", "exit 5"])), 5);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
