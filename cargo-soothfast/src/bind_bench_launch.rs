//! Per-language launch for `bind bench`, run after `bind build` has
//! produced the package a script measures against.

use std::path::Path;
use std::process::Command;

use soothfast_bind::BindKind;

/// Why a script didn't run.
pub enum LaunchError {
    /// A required tool, or a build artifact it depends on, isn't there;
    /// the caller skips the entry.
    Missing(String),
    /// Setup ran but failed for a real reason; the caller fails the entry.
    Failed(String),
}

/// The tool `lang`'s bench launch needs that `bind build` doesn't already
/// guarantee, missing from this machine — name and install hint, in the
/// order to check them. Empty for a language `bind bench` never skips on.
fn required_tools(lang: BindKind) -> &'static [(&'static str, &'static str)] {
    match lang {
        BindKind::Python => &[("maturin", "pip install maturin, or uv tool install maturin")],
        BindKind::Node => &[("node", "install Node.js (https://nodejs.org)")],
        BindKind::Go => &[("go", "install Go — https://go.dev/doc/install")],
        BindKind::Java => &[("java", "install a JDK")],
        BindKind::Kotlin => &[(
            "kotlin",
            "install the Kotlin compiler — https://kotlinlang.org/docs/command-line.html",
        )],
        BindKind::R => &[("Rscript", "install R (https://www.r-project.org)")],
        BindKind::CAbi | BindKind::Wasm | BindKind::Ruby | BindKind::Cpp | BindKind::Lua => &[],
    }
}

/// The first of `lang`'s required tools that isn't on this machine.
pub fn missing_tool(lang: BindKind) -> Option<(&'static str, &'static str)> {
    required_tools(lang)
        .iter()
        .find(|(tool, _)| Command::new(tool).arg("--version").output().is_err())
        .copied()
}

/// Build the command that runs `script` for `lang`, with `glue` (the
/// `[[bind]] out` directory) and `artifacts` (what `bind build` just
/// reported it produced there) set up so the language finds them.
pub fn launch(
    lang: BindKind,
    glue: &Path,
    script: &Path,
    artifacts: &[String],
) -> Result<Command, LaunchError> {
    match lang {
        BindKind::Python => python(glue, script, artifacts),
        BindKind::Node => Ok(node(glue, script)),
        BindKind::Go => Ok(go(glue, script)),
        BindKind::Java => jvm(glue, script, "java", artifacts),
        BindKind::Kotlin => jvm(glue, script, "kotlin", artifacts),
        BindKind::R => Ok(r(glue, script)),
        BindKind::CAbi => Ok(direct(glue, script)),
        BindKind::Wasm | BindKind::Ruby | BindKind::Cpp | BindKind::Lua => {
            Err(LaunchError::Failed(format!(
                "bind bench has no launcher for {} yet",
                lang.name()
            )))
        }
    }
}

/// The first of `artifacts` ending in `suffix`.
fn find_artifact<'a>(artifacts: &'a [String], suffix: &str) -> Option<&'a str> {
    artifacts
        .iter()
        .map(String::as_str)
        .find(|a| a.ends_with(suffix))
}

/// Run a setup step to completion, capturing its output instead of
/// inheriting stdio — so its own tool's progress noise never lands on
/// `bind bench`'s stdout, which is either a table or a JSON stream.
fn run_setup(mut cmd: Command, label: &str) -> Result<(), LaunchError> {
    let out = cmd.output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => LaunchError::Missing(format!("`{label}` not found")),
        _ => LaunchError::Failed(format!("cannot run {label}: {e}")),
    })?;
    if out.status.success() {
        Ok(())
    } else {
        Err(LaunchError::Failed(format!(
            "{label} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Install the wheel `bind build` just produced into a scratch venv under
/// `target/` (already gitignored by every generated package), then run the
/// script with that venv's interpreter. Built with `python3 -m venv` and
/// installed with that venv's own `pip` — both ship with every CPython,
/// unlike `uv`, which a bare CI runner may not have.
fn python(glue: &Path, script: &Path, artifacts: &[String]) -> Result<Command, LaunchError> {
    let wheel = find_artifact(artifacts, ".whl")
        .ok_or_else(|| LaunchError::Missing("no wheel among bind build's artifacts".to_string()))?;
    let venv = glue.join("target/.bind-bench-venv");
    let python_bin = venv.join("bin/python");
    if !python_bin.exists() {
        // The venv's own interpreter is whichever python3 built it, the
        // same one maturin build defaults to with no --interpreter override.
        let interpreter = python3_executable()?;
        let mut make_venv = Command::new(&interpreter);
        make_venv.args(["-m", "venv"]).arg(&venv);
        run_setup(make_venv, "python3 -m venv")?;
    }
    let mut install = Command::new(venv.join("bin/pip"));
    install.arg("install").arg("--force-reinstall").arg(wheel);
    run_setup(install, "pip install")?;
    let mut cmd = Command::new(python_bin);
    cmd.arg(script).current_dir(glue);
    Ok(cmd)
}

/// The absolute path `python3` resolves to on `PATH` — what `maturin
/// build` targets by default with no `--interpreter` override.
fn python3_executable() -> Result<String, LaunchError> {
    let out = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => LaunchError::Missing("`python3` not found".to_string()),
            _ => LaunchError::Failed(format!("cannot run python3: {e}")),
        })?;
    if !out.status.success() {
        return Err(LaunchError::Failed("`python3 -c ...` failed".to_string()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `node <script>`, with the glue directory (where the built addon and its
/// JS loader live) as both cwd and `NODE_PATH`.
fn node(glue: &Path, script: &Path) -> Command {
    let mut cmd = Command::new("node");
    cmd.arg(script).current_dir(glue).env("NODE_PATH", glue);
    cmd
}

/// `go run <script>`, inside the package dir `go.mod` lives in. cgo needs
/// `CGO_ENABLED=1` the same way `bind build`'s own `go vet`/`go build` do.
fn go(glue: &Path, script: &Path) -> Command {
    let mut cmd = Command::new("go");
    cmd.arg("run")
        .arg(script)
        .current_dir(glue)
        .env("CGO_ENABLED", "1");
    cmd
}

/// Single-file source launch (`java Bench.java`), with the jar `bind
/// build` reported on the classpath. Kotlin shares this: a `.kts` script
/// runs the same way through the `kotlin` launcher. No jar among the
/// artifacts means the packaging tool (`javac`/`kotlinc` plus `jar`) is
/// missing, not that the entry is broken.
fn jvm(
    glue: &Path,
    script: &Path,
    tool: &str,
    artifacts: &[String],
) -> Result<Command, LaunchError> {
    let jar = find_artifact(artifacts, ".jar")
        .ok_or_else(|| LaunchError::Missing("no jar among bind build's artifacts".to_string()))?;
    let mut cmd = Command::new(tool);
    cmd.args(["-cp", jar]).arg(script).current_dir(glue);
    Ok(cmd)
}

/// `Rscript <script>`, with the installed library first on `R_LIBS`.
fn r(glue: &Path, script: &Path) -> Command {
    let mut cmd = Command::new("Rscript");
    let lib = glue.join("target/rlib");
    let r_libs = match std::env::var("R_LIBS") {
        Ok(existing) => format!("{}:{existing}", lib.display()),
        Err(_) => lib.display().to_string(),
    };
    cmd.arg(script).current_dir(glue).env("R_LIBS", r_libs);
    cmd
}

/// The script itself, run directly: the only shape that fits a language
/// with no scripting story of its own — an already-compiled harness.
fn direct(glue: &Path, script: &Path) -> Command {
    let mut cmd = Command::new(script);
    cmd.current_dir(glue);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_artifact_matches_by_suffix_among_several() {
        let artifacts = vec![
            "/out/target/soothfast-stats.jar".to_string(),
            "/out/target/release/libio_soothfast_stats.so".to_string(),
        ];
        assert_eq!(
            find_artifact(&artifacts, ".jar"),
            Some("/out/target/soothfast-stats.jar")
        );
        assert_eq!(find_artifact(&artifacts, ".whl"), None);
    }

    #[test]
    fn jvm_launch_fails_missing_without_a_jar() {
        let artifacts = vec!["/out/target/release/libio_soothfast_stats.so".to_string()];
        let err = jvm(
            Path::new("/out"),
            Path::new("/out/bench.java"),
            "java",
            &artifacts,
        )
        .expect_err("no jar to launch with");
        assert!(matches!(err, LaunchError::Missing(_)));
    }

    #[test]
    fn every_required_tool_has_an_install_hint() {
        for lang in BindKind::ALL {
            for (tool, hint) in required_tools(*lang) {
                assert!(!tool.is_empty());
                assert!(!hint.is_empty());
            }
        }
    }
}
