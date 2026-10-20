//! Running test executables the way `cargo test` would.

use std::io;
use std::path::PathBuf;
use std::process::Command;

use crate::invoke;
use crate::testbuild::TestBin;
use crate::testsel::Package;

#[cfg(target_os = "macos")]
const LIB_PATH_VAR: &str = "DYLD_FALLBACK_LIBRARY_PATH";
#[cfg(not(target_os = "macos"))]
const LIB_PATH_VAR: &str = "LD_LIBRARY_PATH";

/// The parts of cargo's test environment that do not depend on the package.
pub struct Runner {
    target_tmp: PathBuf,
    libdir: PathBuf,
}

impl Runner {
    /// Resolve the target directory and the toolchain's library dir.
    pub fn new() -> io::Result<Runner> {
        let target_tmp = invoke::target_dir(None)?.join("tmp");
        std::fs::create_dir_all(&target_tmp)?;
        let out = Command::new("rustc")
            .args(["--print", "target-libdir"])
            .output()?;
        Ok(Runner {
            target_tmp,
            libdir: PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()),
        })
    }

    /// Run one test executable in its package dir with cargo's environment,
    /// output streaming. Whether it exited successfully.
    pub fn run(&self, bin: &TestBin, pkg: &Package, run_args: &[String]) -> io::Result<bool> {
        let mut libs = vec![self.libdir.clone()];
        libs.extend(bin.exe.parent().map(PathBuf::from));
        libs.extend(
            std::env::var_os(LIB_PATH_VAR)
                .iter()
                .flat_map(std::env::split_paths),
        );
        let mut cmd = Command::new(&bin.exe);
        cmd.args(run_args)
            .current_dir(&pkg.manifest_dir)
            .envs(pkg.env.iter().cloned())
            .env("CARGO_CRATE_NAME", bin.target.replace('-', "_"))
            .env("CARGO_PRIMARY_PACKAGE", "1")
            .env("CARGO_TARGET_TMPDIR", &self.target_tmp)
            .env(
                LIB_PATH_VAR,
                std::env::join_paths(libs).map_err(io::Error::other)?,
            );
        Ok(cmd.status()?.success())
    }
}

/// `cargo test --doc` for `names`; executables cannot be cached, so doc tests
/// always run.
pub fn doc_tests(names: &[String], cargo_args: &[String], run_args: &[String]) -> io::Result<bool> {
    let mut cmd = Command::new("cargo");
    cmd.args(["test", "--doc"]);
    for n in names {
        cmd.args(["-p", n]);
    }
    cmd.args(cargo_args);
    if !run_args.is_empty() {
        cmd.arg("--").args(run_args);
    }
    Ok(cmd.status()?.success())
}

/// Whether cargo flags already pick test targets, which `--doc` refuses to
/// combine with.
pub fn narrows_targets(cargo_args: &[String]) -> bool {
    const FLAGS: &[&str] = &[
        "--lib",
        "--bin",
        "--bins",
        "--test",
        "--tests",
        "--example",
        "--examples",
        "--bench",
        "--benches",
        "--all-targets",
    ];
    cargo_args.iter().any(|a| FLAGS.contains(&a.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn target_selecting_flags_are_detected() {
        assert!(narrows_targets(&args(&["--release", "--lib"])));
        assert!(!narrows_targets(&args(&["--all-features", "--release"])));
    }

    #[test]
    fn a_test_runs_in_its_package_dir_with_cargos_env() {
        let dir = std::env::temp_dir().join(format!("soothfast-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("probe.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n[ \"$CARGO_MANIFEST_DIR\" = \"$PWD\" ] && [ \"$CARGO_CRATE_NAME\" = my_probe ] && [ \"$1\" = want ]\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let pkg = Package {
            name: "p".into(),
            dir: String::new(),
            manifest_dir: dir.clone(),
            deps: Vec::new(),
            env: vec![("CARGO_MANIFEST_DIR".into(), dir.display().to_string())],
        };
        let bin = TestBin {
            package: "p".into(),
            target: "my-probe".into(),
            kind: "test".into(),
            exe: script,
        };
        let runner = Runner {
            target_tmp: dir.clone(),
            libdir: dir.clone(),
        };
        assert!(runner.run(&bin, &pkg, &args(&["want"])).unwrap());
        assert!(!runner.run(&bin, &pkg, &args(&["other"])).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
