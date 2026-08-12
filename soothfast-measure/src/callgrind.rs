//! Callgrind backend: fully deterministic Ir counts for PMU-less
//! environments (default-seccomp containers, CI VMs). The runner re-executes
//! itself under valgrind with `--callgrind-exec <id> --iters K`; per-iteration
//! Ir = (run at 2K − run at K) / K, which cancels startup + setup exactly —
//! no client requests, no toggle-collect fragility. K > 1 so the window
//! averages: see the constant below for why a single iteration is not enough.

use std::path::PathBuf;
use std::process::Command;

/// Disable runtime AVX-512 dispatch in the child: valgrind's VEX can't
/// decode EVEX. Helps hosts whose *loader* is decodable; a loader compiled
/// with AVX-512 (CachyOS x86-64-v4 glibc) still SIGILLs — probe catches it.
const GLIBC_TUNABLES: (&str, &str) = (
    "GLIBC_TUNABLES",
    "glibc.cpu.hwcaps=-AVX512F,-AVX512VL,-AVX512BW,-AVX512DQ,-AVX512CD,-AVX512_VBMI,-AVX512_VBMI2,-AVX512_VNNI,-AVX512_BITALG,-AVX512_VPOPCNTDQ",
);

/// Can valgrind actually run THIS binary? `valgrind --version` is not
/// enough: an AVX-512-compiled glibc loader SIGILLs before main().
pub fn probe() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = out_file("probe");
    let result = Command::new("valgrind")
        .env(GLIBC_TUNABLES.0, GLIBC_TUNABLES.1)
        .args([
            "--tool=callgrind",
            &format!("--callgrind-out-file={}", out.display()),
        ])
        .arg(&exe)
        .arg("--list")
        .output();
    let _ = std::fs::remove_file(&out);
    match result {
        Err(e) => Err(format!("no runnable valgrind on PATH: {e}")),
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let hint = if stderr.contains("unhandled instruction")
                || o.status.to_string().contains("SIGILL")
            {
                " (glibc compiled with AVX-512 — valgrind cannot decode this host's loader; use the perfcnt backend or a vanilla-glibc container)"
            } else {
                ""
            };
            Err(format!(
                "valgrind cannot run this binary: {}{hint}",
                o.status
            ))
        }
    }
}

fn out_file(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "soothfast-callgrind-{}-{tag}.out",
        std::process::id()
    ))
}

/// Run this bench binary under callgrind executing `id` for `iters`
/// iterations; return total Ir (and the out-file path for annotation).
fn run_ir(id: &str, iters: u64, tag: &str) -> Result<(u64, PathBuf), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = out_file(tag);
    let status = Command::new("valgrind")
        .env(GLIBC_TUNABLES.0, GLIBC_TUNABLES.1)
        .args([
            "--tool=callgrind",
            "--compress-strings=no",
            "--compress-pos=no",
            &format!("--callgrind-out-file={}", out.display()),
        ])
        .arg(&exe)
        .args(["--callgrind-exec", id, "--iters", &iters.to_string()])
        .output()
        .map_err(|e| format!("failed to spawn valgrind: {e}"))?;
    if !status.status.success() {
        return Err(format!(
            "valgrind failed ({}): {}",
            status.status,
            String::from_utf8_lossy(&status.stderr)
        ));
    }
    let text = std::fs::read_to_string(&out).map_err(|e| e.to_string())?;
    let summary = text
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("summary: "))
        .ok_or("no summary line in callgrind output")?;
    let ir: u64 = summary
        .split_whitespace()
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or("unparseable summary line")?;
    Ok((ir, out))
}

/// Iterations in the low run; the high run does twice this.
///
/// K must exceed 1. `gate` compares two builds run from different working
/// directories — the reference side lives in a worktree whose path is ~60
/// characters longer — and that difference in environment size alone shifts
/// heap layout, hence the instruction count of allocator work inside the
/// measured region. Measured directly: the same binary run from a 42- and a
/// 115-character path differs by 0.08%. At K=1 the subtraction reports one
/// iteration, so that offset lands at full weight; averaging over K spreads it
/// to 1/K. Valgrind process startup dominates each run, so raising K costs far
/// less than it looks like it does.
const K: u64 = 10;

/// Per-iteration Ir for one item.
pub fn measure(id: &str) -> Result<u64, String> {
    let (low, f1) = run_ir(id, K, "k1")?;
    let (high, f2) = run_ir(id, 2 * K, "k2")?;
    for f in [f1, f2] {
        let _ = std::fs::remove_file(f);
    }
    Ok(high.saturating_sub(low) / K)
}

/// Human triage report: top self-cost functions for one item's workload.
pub fn annotate(id: &str) -> Result<String, String> {
    let (total, path) = run_ir(id, 1, "annotate")?;
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&path);

    // Callgrind format (uncompressed): "fn=<name>" starts a function block;
    // following "<pos> <cost>" lines are its self costs.
    let mut costs: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut current: Option<String> = None;
    // The cost line after "calls=" is the call's inclusive cost — not self.
    let mut skip_next_cost = false;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("fn=") {
            current = Some(name.to_string());
            skip_next_cost = false;
        } else if line.starts_with("calls=") {
            skip_next_cost = true;
        } else if line.starts_with(|c: char| c.is_ascii_digit()) {
            if skip_next_cost {
                skip_next_cost = false;
            } else if let Some(fn_name) = &current
                && let Some(cost) = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|v| v.parse::<u64>().ok())
            {
                *costs.entry(fn_name.clone()).or_default() += cost;
            }
        }
    }
    let mut ranked: Vec<(String, u64)> = costs.into_iter().collect();
    ranked.sort_by_key(|(_, cost)| std::cmp::Reverse(*cost));

    let mut report = format!("triage: {id}  (total Ir = {total}, top self-cost functions)\n");
    for (name, cost) in ranked.iter().take(20) {
        let pct = *cost as f64 / total.max(1) as f64 * 100.0;
        report.push_str(&format!("{cost:>14} Ir  {pct:5.1}%  {name}\n"));
    }
    Ok(report)
}
