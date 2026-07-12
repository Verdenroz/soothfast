//! Buildcost backend (CLI-side — no bench binary involved): compile time and
//! artifact size per declared feature combination. Size gates hard; compile
//! time is soft (see gate.rs thresholds). Combos are a declared matrix
//! (`--features-matrix "default;full;a,b"`), never the powerset.

use std::process::{Command, Stdio};
use std::time::Instant;

use crate::invoke::{ItemMetrics, Run};

/// Measure each combo: `cargo clean -p PKG` then a timed release build,
/// artifact size taken as the largest emitted file for the package.
pub fn measure(pkg: &str, matrix: &str) -> Result<Run, String> {
    measure_in(pkg, matrix, None)
}

/// Like [`measure`], but built from another directory (a merge-base
/// worktree) so `gate --backend buildcost --against-ref` can compare.
pub fn measure_in(pkg: &str, matrix: &str, dir: Option<&std::path::Path>) -> Result<Run, String> {
    let mut run = Run::default();
    for combo in matrix.split(';').map(str::trim).filter(|c| !c.is_empty()) {
        let mut clean_cmd = Command::new("cargo");
        clean_cmd.args(["clean", "--release", "-p", pkg]);
        if let Some(d) = dir {
            clean_cmd.current_dir(d);
        }
        let clean = clean_cmd.status().map_err(|e| e.to_string())?;
        if !clean.success() {
            return Err(format!("cargo clean failed for {pkg}"));
        }

        let mut cmd = Command::new("cargo");
        cmd.args([
            "build",
            "--release",
            "-p",
            pkg,
            "--message-format",
            "json-render-diagnostics",
        ]);
        if let Some(d) = dir {
            cmd.current_dir(d);
        }
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
