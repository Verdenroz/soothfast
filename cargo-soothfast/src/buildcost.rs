//! Buildcost backend (CLI-side — no bench binary involved): compile time and
//! artifact size per declared feature combination. Size gates hard; compile
//! time is soft (see gate.rs thresholds). Combos are a declared matrix
//! (`--features-matrix "default;full;a,b"`), never the powerset.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::invoke::{ItemMetrics, Run};

/// Measure each combo: `cargo clean -p PKG` then a timed release build,
/// artifact size taken as the largest emitted file for the package.
pub fn measure(pkg: &str, matrix: &str, target_dir: Option<&Path>) -> Result<Run, String> {
    measure_in(pkg, matrix, None, target_dir)
}

/// Like [`measure`], but built from another directory (a merge-base
/// worktree) so `gate --backend buildcost --against-ref` can compare.
pub fn measure_in(
    pkg: &str,
    matrix: &str,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
) -> Result<Run, String> {
    warm_dependencies(pkg, dir, target_dir)?;
    let mut run = Run::default();
    for combo in matrix.split(';').map(str::trim).filter(|c| !c.is_empty()) {
        let clean = cargo(&["clean", "--release", "-p", pkg], dir, target_dir)
            .status()
            .map_err(|e| e.to_string())?;
        if !clean.success() {
            return Err(format!("cargo clean failed for {pkg}"));
        }

        let mut cmd = cargo(
            &[
                "build",
                "--release",
                "-p",
                pkg,
                "--message-format",
                "json-render-diagnostics",
            ],
            dir,
            target_dir,
        );
        match combo {
            "default" => {}
            "none" => {
                cmd.arg("--no-default-features");
            }
            features => {
                cmd.args(["--features", features]);
            }
        }
        cmd.stdout(Stdio::piped());

        let t0 = Instant::now();
        let out = cmd.output().map_err(|e| e.to_string())?;
        let build_ms = t0.elapsed().as_millis() as u64;
        if !out.status.success() {
            return Err(format!(
                "cargo build failed for {pkg} with features {combo:?}"
            ));
        }

        // Largest artifact emitted for this package (rlib for libs, bin for
        // bins), matched on target name — package_id formats vary by source.
        let stdout = String::from_utf8_lossy(&out.stdout);
        let size_bytes = stdout
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["reason"] == "compiler-artifact")
            .filter(|v| {
                v["target"]["name"]
                    .as_str()
                    .is_some_and(|n| n == pkg || n == pkg.replace('-', "_"))
            })
            .flat_map(|v| {
                v["filenames"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|f| f.as_str().map(String::from))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .filter_map(|f| std::fs::metadata(f).ok().map(|m| m.len()))
            .max()
            .unwrap_or(0);

        let id = format!("buildcost::{pkg}::{combo}");
        println!("{id:<44} buildcost build_ms={build_ms} size_bytes={size_bytes}");
        run.items.insert(
            id,
            ItemMetrics {
                build_ms: Some(build_ms),
                size_bytes: Some(size_bytes),
                ..Default::default()
            },
        );
    }
    Ok(run)
}

fn cargo(args: &[&str], dir: Option<&Path>, target_dir: Option<&Path>) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.args(args);
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    if let Some(td) = target_dir {
        cmd.env("CARGO_TARGET_DIR", td);
    }
    cmd
}

/// Build once untimed. `clean -p` drops only this package, so on a cold
/// target dir the dependencies would otherwise compile inside the timed
/// window and the number would measure the machine's history.
fn warm_dependencies(
    pkg: &str,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
) -> Result<(), String> {
    let warm = cargo(&["build", "--release", "-p", pkg], dir, target_dir)
        .status()
        .map_err(|e| e.to_string())?;
    if warm.success() {
        return Ok(());
    }
    Err(format!("warmup build failed for {pkg}"))
}
