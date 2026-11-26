//! `llvm-profdata` and `llvm-cov`, from the `llvm-tools` rustup component.

use std::path::PathBuf;
use std::process::Command;

pub struct Tools {
    pub profdata: PathBuf,
    pub cov: PathBuf,
}

/// The tools beside the active compiler.
pub fn locate() -> Result<Tools, String> {
    let out = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .map_err(|e| format!("rustc: {e}"))?;
    let sysroot = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let host = host_triple()?;
    let beside = sysroot.join("lib/rustlib").join(host).join("bin");
    let find = |name: &str| {
        let path = beside.join(name);
        path.is_file().then_some(path).ok_or_else(|| {
            format!(
                "{name} not found in {}; install it with `rustup component add llvm-tools`",
                beside.display()
            )
        })
    };
    Ok(Tools {
        profdata: find("llvm-profdata")?,
        cov: find("llvm-cov")?,
    })
}

/// The compiler's host triple.
pub fn host_triple() -> Result<String, String> {
    crate::buildstamp::rustc_vv(None)
        .map_err(|e| e.to_string())?
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(String::from))
        .ok_or_else(|| "rustc -vV names no host".into())
}
