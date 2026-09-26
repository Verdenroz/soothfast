//! Callgrind backend: fully deterministic Ir counts for PMU-less
//! environments (default-seccomp containers, CI VMs). The runner re-executes
//! itself under valgrind with `--callgrind-exec <id> --iters K`; per-iteration
//! Ir = (run at 2K − run at K) / K, which cancels startup + setup exactly —
//! no client requests, no toggle-collect fragility. K > 1 so the window
//! averages: see the constant below for why a single iteration is not enough.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

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

/// What decides callgrind's Ir besides the binary: the CPU valgrind shows the
/// guest (not the host's, which valgrind replaces with a synthesized one), the
/// glibc the guest loads, and the valgrind counting it.
#[derive(Debug, Clone, PartialEq)]
pub struct Guest {
    pub cpu: String,
    pub glibc: String,
    pub valgrind: String,
}

/// Ask this binary, under valgrind as a measurement runs it, what it sees.
/// `None` where the guest cannot say (not x86_64, not glibc).
pub fn guest() -> Option<Guest> {
    let exe = std::env::current_exe().ok()?;
    let out = out_file("guest");
    let run = Command::new("valgrind")
        .env(GLIBC_TUNABLES.0, GLIBC_TUNABLES.1)
        .args([
            "--tool=callgrind",
            &format!("--callgrind-out-file={}", out.display()),
        ])
        .arg(&exe)
        .arg("--guest-view")
        .output();
    let _ = std::fs::remove_file(&out);
    let run = run.ok().filter(|o| o.status.success())?;
    let stdout = String::from_utf8_lossy(&run.stdout);
    let (cpu, glibc) = stdout
        .lines()
        .find_map(|l| l.strip_prefix("guest "))?
        .split_once(' ')?;
    let version = Command::new("valgrind").arg("--version").output().ok()?;
    Some(Guest {
        cpu: cpu.to_string(),
        glibc: glibc.trim().to_string(),
        valgrind: String::from_utf8_lossy(&version.stdout).trim().to_string(),
    })
}

/// The guest half of [`guest`]: prints `guest <cpuid hash> <glibc version>`,
/// or nothing where either is unknown.
pub fn print_guest_view() {
    if let (Some(cpu), Some(glibc)) = (cpuid_hash(), glibc_version()) {
        println!("guest {cpu} {glibc}");
    }
}

/// Every CPUID leaf a program dispatches on: vendor, feature bits, cache
/// descriptors. Leaf 1's APIC ID differs per core, so it is masked out.
#[cfg(target_arch = "x86_64")]
fn cpuid_hash() -> Option<String> {
    #[allow(unused_unsafe)]
    let cpuid = |leaf: u32, sub: u32| unsafe { std::arch::x86_64::__cpuid_count(leaf, sub) };
    let mut words = Vec::new();
    let max = cpuid(0, 0).eax;
    let mut leaves = vec![(0, 0), (1, 0), (7, 0)];
    for sub in 0..16 {
        if max < 4 || cpuid(4, sub).eax & 0x1f == 0 {
            break;
        }
        leaves.push((4, sub));
    }
    let ext_max = cpuid(0x8000_0000, 0).eax;
    leaves.extend([(0x8000_0000, 0), (0x8000_0001, 0)]);
    for (leaf, sub) in leaves {
        let in_range = if leaf >= 0x8000_0000 {
            leaf <= ext_max
        } else {
            leaf <= max
        };
        if !in_range {
            continue;
        }
        let r = cpuid(leaf, sub);
        let ebx = if leaf == 1 {
            r.ebx & 0x00ff_ffff
        } else {
            r.ebx
        };
        words.extend([leaf, sub, r.eax, ebx, r.ecx, r.edx]);
    }
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    Some(format!("{:016x}", soothfast_registry::fnv1a(&bytes)))
}

#[cfg(not(target_arch = "x86_64"))]
fn cpuid_hash() -> Option<String> {
    None
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn glibc_version() -> Option<String> {
    let v = unsafe { std::ffi::CStr::from_ptr(libc::gnu_get_libc_version()) };
    v.to_str().ok().map(String::from)
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn glibc_version() -> Option<String> {
    None
}

// The sequence number keeps concurrent measurements (measure_all) from
// colliding on a pid-keyed name.
fn out_file(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "soothfast-callgrind-{}-{}-{tag}.out",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
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

// Each measurement holds a live valgrind child with a real memory footprint,
// so the pool is capped rather than sized to the machine.
const MAX_PARALLEL: usize = 4;

/// Per-iteration Ir for many items, measured concurrently (Ir counts are
/// timing-independent). Results come back in input order.
pub fn measure_all(ids: &[&str]) -> Vec<Result<u64, String>> {
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(MAX_PARALLEL)
        .min(ids.len());
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<Result<u64, String>>>> =
        ids.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(id) = ids.get(i) else { break };
                    *slots[i].lock().unwrap() = Some(measure(id));
                }
            });
        }
    });
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .unwrap()
                .expect("worker filled every slot")
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::out_file;

    #[test]
    fn out_files_never_collide_even_with_the_same_tag() {
        assert_ne!(out_file("k1"), out_file("k1"));
    }
}
