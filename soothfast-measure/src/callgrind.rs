//! Callgrind backend: fully deterministic Ir counts for PMU-less
//! environments (default-seccomp containers, CI VMs). The runner re-executes
//! itself under valgrind with `--callgrind-exec <id> --iters K`; per-iteration
//! Ir = (run at 2K − run at K) / K, which cancels startup + setup exactly —
//! no client requests, no toggle-collect fragility. A third run at 3K gives a
//! second, disjoint window: the two must agree, or the workload has not
//! reached steady state and no per-iteration cost is well defined.

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

/// Iterations per window. Only a runtime knob: the convergence check below is
/// what makes the result trustworthy, so K is not sized to hide anything.
const K: u64 = 4;

/// How far the two window estimates may differ, in per-mille of the larger.
/// In steady state both windows execute identical work and callgrind is
/// deterministic, so they should agree exactly; this only absorbs rounding
/// from the two integer divisions.
const CONVERGENCE_TOLERANCE_PER_MILLE: u64 = 2;

/// Per-iteration Ir for one item.
///
/// Two disjoint windows are measured — iterations K..2K and 2K..3K — and must
/// agree. `(2K − K)/K` alone assumes per-iteration cost is already constant; a
/// workload still reaching steady state (an allocator growing its arena, say)
/// violates that, and a single window would silently report whichever cost
/// happened to land inside it. Disagreement is reported rather than averaged
/// away, because the number would not mean what the gate assumes it means.
pub fn measure(id: &str) -> Result<u64, String> {
    let (a, f1) = run_ir(id, K, "k1")?;
    let (b, f2) = run_ir(id, 2 * K, "k2")?;
    let (c, f3) = run_ir(id, 3 * K, "k3")?;
    for f in [f1, f2, f3] {
        let _ = std::fs::remove_file(f);
    }

    let first = b.saturating_sub(a) / K;
    let second = c.saturating_sub(b) / K;
    let (lo, hi) = (first.min(second), first.max(second));
    if hi.saturating_sub(lo) * 1000 > hi.saturating_mul(CONVERGENCE_TOLERANCE_PER_MILLE) {
        return Err(format!(
            "{id}: Ir has not converged ({first} then {second} per iteration); \
             the workload is still warming up, so no per-iteration cost is well defined"
        ));
    }
    // The later window is the more thoroughly warmed of the two.
    Ok(second)
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
