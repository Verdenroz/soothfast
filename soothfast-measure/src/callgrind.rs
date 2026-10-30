//! Callgrind backend: fully deterministic Ir counts for PMU-less
//! environments (default-seccomp containers, CI VMs).
//!
//! One valgrind process runs per item, as `--callgrind-exec <id> --iters K
//! --callgrind-window`, with instrumentation on from the start. After setup
//! the runner executes K iterations, requests a stats dump, executes K more
//! and requests another. A dump zeroes the counters, so the second dump file
//! holds exactly K iterations and per-iteration Ir is its summary / K. Setup
//! runs once under valgrind instead of twice, which fixture-heavy items need.
//! The request is the target's magic instruction sequence (see `clientreq`).
//!
//! Where the sequence is unavailable, or with `SOOTHFAST_CALLGRIND_TWO_PROCESS=1`,
//! the runner executes twice, at `--iters K` and `--iters 2K`, and
//! per-iteration Ir is (2K − K) / K, which cancels startup and setup. The two
//! methods count differently (teardown, the request itself), so [`Method`]
//! belongs in every cache key.
//!
//! The executed function set is the union of every dump the process wrote:
//! startup, setup, the iterations and teardown. K > 1 so the window
//! averages: see the constant below for why a single iteration is not enough.

use crate::clientreq;
use std::path::{Path, PathBuf};
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
    /// The version glibc reports, for reading. Distro rebuilds keep it.
    pub glibc: String,
    /// Digest of the libc file the guest loaded, which a rebuild does change.
    pub libc: String,
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
    let mut view = stdout
        .lines()
        .find_map(|l| l.strip_prefix("guest "))?
        .splitn(3, ' ');
    let (cpu, glibc, libc_path) = (view.next()?, view.next()?, view.next()?);
    let libc = std::fs::read(libc_path.trim()).ok()?;
    let version = Command::new("valgrind").arg("--version").output().ok()?;
    Some(Guest {
        cpu: cpu.to_string(),
        glibc: glibc.to_string(),
        libc: format!("{:016x}", soothfast_registry::fnv1a(&libc)),
        valgrind: String::from_utf8_lossy(&version.stdout).trim().to_string(),
    })
}

/// The guest half of [`guest`]: prints `guest <cpuid hash> <glibc version>
/// <loaded libc path>`, or nothing where any is unknown. The path is hashed
/// outside valgrind, where reading the file costs nothing.
pub fn print_guest_view() {
    if let (Some(cpu), Some((version, path))) = (cpuid_hash(), loaded_glibc()) {
        println!("guest {cpu} {version} {path}");
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

/// The glibc version and the path of the libc this process loaded.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn loaded_glibc() -> Option<(String, String)> {
    use std::ffi::CStr;
    let version = unsafe { CStr::from_ptr(libc::gnu_get_libc_version()) };
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    let symbol = libc::gnu_get_libc_version as *const libc::c_void;
    if unsafe { libc::dladdr(symbol, &mut info) } == 0 || info.dli_fname.is_null() {
        return None;
    }
    let path = unsafe { CStr::from_ptr(info.dli_fname) };
    Some((
        version.to_str().ok()?.to_string(),
        path.to_str().ok()?.to_string(),
    ))
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn loaded_glibc() -> Option<(String, String)> {
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
/// iterations; return the text of every dump it wrote, in order. `mangled`
/// keeps the symbol-table spelling of function names instead of demangling
/// them. `window` makes the runner request a dump after each K iterations.
fn run_valgrind(
    id: &str,
    iters: u64,
    window: bool,
    tag: &str,
    mangled: bool,
) -> Result<Vec<String>, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = out_file(tag);
    let mut cmd = Command::new("valgrind");
    cmd.env(GLIBC_TUNABLES.0, GLIBC_TUNABLES.1)
        .args([
            "--tool=callgrind",
            "--compress-strings=no",
            "--compress-pos=no",
            &format!("--demangle={}", if mangled { "no" } else { "yes" }),
            &format!("--callgrind-out-file={}", out.display()),
        ])
        .arg(&exe)
        .args(["--callgrind-exec", id, "--iters", &iters.to_string()]);
    if window {
        cmd.arg("--callgrind-window");
    }
    let status = cmd
        .output()
        .map_err(|e| format!("failed to spawn valgrind: {e}"))?;
    let dumps = take_dumps(&out);
    if !status.status.success() {
        return Err(format!(
            "valgrind failed ({}): {}",
            status.status,
            String::from_utf8_lossy(&status.stderr)
        ));
    }
    dumps
}

/// Read and delete the dump files callgrind wrote for `out`, in dump order:
/// `out.1`, `out.2`, ... for each requested dump, then `out` for the exit dump.
fn take_dumps(out: &Path) -> Result<Vec<String>, String> {
    let name = out.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let dir = out.parent().unwrap_or(out);
    let mut parts: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name();
            let rest = file.to_str()?.strip_prefix(name)?;
            let part = match rest.strip_prefix('.') {
                Some(n) => n.parse().ok()?,
                None if rest.is_empty() => u64::MAX,
                None => return None,
            };
            Some((part, entry.path()))
        })
        .collect();
    parts.sort();
    parts
        .into_iter()
        .map(|(_, path)| {
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string());
            let _ = std::fs::remove_file(&path);
            text
        })
        .collect()
}

/// Total Ir of one dump file, from its summary line.
fn summary_ir(text: &str) -> Result<u64, String> {
    text.lines()
        .rev()
        .find_map(|l| l.strip_prefix("summary: "))
        .ok_or("no summary line in callgrind output")?
        .split_whitespace()
        .next()
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| "unparseable summary line".into())
}

/// Iterations in the measured window; the two-process method runs K and 2K.
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

/// One item's callgrind result: per-iteration Ir, and every function the
/// process executed as sorted, deduplicated `(object, mangled symbol)`.
pub struct Measurement {
    /// Instructions per iteration.
    pub ir: u64,
    /// Executed `(object, mangled symbol)` pairs.
    pub functions: Vec<(String, String)>,
}

/// How per-iteration Ir is taken from valgrind runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// One process; a dump request brackets a K-iteration window.
    OneProcess,
    /// Two processes, at K and 2K iterations.
    TwoProcess,
}

impl Method {
    /// The method this binary measures with.
    pub fn current() -> Method {
        let forced = std::env::var_os("SOOTHFAST_CALLGRIND_TWO_PROCESS").is_some_and(|v| v == "1");
        if clientreq::SUPPORTED && !forced {
            Method::OneProcess
        } else {
            Method::TwoProcess
        }
    }

    /// The spelling cache keys and the env line carry.
    pub fn name(self) -> &'static str {
        match self {
            Method::OneProcess => "one-process",
            Method::TwoProcess => "two-process",
        }
    }
}

/// Per-iteration Ir for one item.
pub fn measure(id: &str) -> Result<Measurement, String> {
    match Method::current() {
        Method::OneProcess => measure_window(id),
        Method::TwoProcess => measure_difference(id),
    }
}

fn measure_window(id: &str) -> Result<Measurement, String> {
    let dumps = run_valgrind(id, K, true, "window", true)?;
    let [_, window, _] = dumps.as_slice() else {
        return Err(format!("expected 3 callgrind dumps, found {}", dumps.len()));
    };
    Ok(Measurement {
        ir: summary_ir(window)? / K,
        functions: executed(&dumps),
    })
}

fn measure_difference(id: &str) -> Result<Measurement, String> {
    let low = run_valgrind(id, K, false, "k1", true)?;
    let high = run_valgrind(id, 2 * K, false, "k2", true)?;
    Ok(Measurement {
        ir: summary_ir(&high[0])?.saturating_sub(summary_ir(&low[0])?) / K,
        functions: executed(&high),
    })
}

// Each measurement holds a live valgrind child with a real memory footprint,
// so the pool is capped rather than sized to the machine.
const MAX_PARALLEL: usize = 4;

/// Per-iteration Ir for many items, measured concurrently (Ir counts are
/// timing-independent). Results come back in input order.
pub fn measure_all(ids: &[&str]) -> Vec<Result<Measurement, String>> {
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(MAX_PARALLEL)
        .min(ids.len());
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<Result<Measurement, String>>>> =
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

/// Self cost per `(object, function)` cost line of an uncompressed callgrind
/// out-file. `fn=` starts a function block, `ob=` sets the object the
/// following blocks belong to, and the cost line after `calls=` is the call's
/// inclusive cost, not self.
fn function_costs(text: &str) -> impl Iterator<Item = (&str, &str, u64)> {
    let (mut object, mut current) = ("", None);
    let mut skip_next_cost = false;
    text.lines().filter_map(move |line| {
        if let Some(ob) = line.strip_prefix("ob=") {
            object = ob;
        } else if let Some(name) = line.strip_prefix("fn=") {
            current = Some(name);
            skip_next_cost = false;
        } else if line.starts_with("calls=") {
            skip_next_cost = true;
        } else if line.starts_with(|c: char| c.is_ascii_digit()) {
            if skip_next_cost {
                skip_next_cost = false;
            } else if let Some(name) = current
                && let Some(cost) = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|v| v.parse::<u64>().ok())
            {
                return Some((object, name, cost));
            }
        }
        None
    })
}

fn executed(dumps: &[String]) -> Vec<(String, String)> {
    let set: std::collections::BTreeSet<(&str, &str)> = dumps
        .iter()
        .flat_map(|text| function_costs(text))
        .map(|(ob, f, _)| (ob, f))
        .collect();
    set.into_iter()
        .map(|(ob, f)| (ob.to_string(), f.to_string()))
        .collect()
}

/// Human triage report: top self-cost functions for one item's workload.
pub fn annotate(id: &str) -> Result<String, String> {
    let dumps = run_valgrind(id, 1, false, "annotate", false)?;
    let text = &dumps[0];
    let total = summary_ir(text)?;
    let mut costs: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for (_, name, cost) in function_costs(text) {
        *costs.entry(name.to_string()).or_default() += cost;
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
    use std::path::PathBuf;

    use super::{executed, function_costs, out_file, summary_ir, take_dumps};

    const OUT: &str = "\
events: Ir
ob=/work/bench
fl=/src/lib.rs
fn=_ZN5bench4hot17h0123456789abcdefE
10 100
cfn=_ZN5bench4cold17h0123456789abcdefE
calls=2 20
11 900
12 5
ob=/lib/ld-linux.so
fn=_dl_relocate
3 40
ob=/work/bench
fn=_ZN5bench4hot17h0123456789abcdefE
13 7
summary: 1052
";

    #[test]
    fn out_files_never_collide_even_with_the_same_tag() {
        assert_ne!(out_file("k1"), out_file("k1"));
    }

    #[test]
    fn self_costs_skip_the_inclusive_line_after_calls() {
        let hot = "_ZN5bench4hot17h0123456789abcdefE";
        let costs: Vec<_> = function_costs(OUT).collect();
        assert_eq!(
            costs,
            [
                ("/work/bench", hot, 100),
                ("/work/bench", hot, 5),
                ("/lib/ld-linux.so", "_dl_relocate", 40),
                ("/work/bench", hot, 7),
            ]
        );
    }

    #[test]
    fn executed_functions_are_sorted_and_deduplicated_per_object() {
        let want: Vec<(String, String)> = vec![
            ("/lib/ld-linux.so".into(), "_dl_relocate".into()),
            (
                "/work/bench".into(),
                "_ZN5bench4hot17h0123456789abcdefE".into(),
            ),
        ];
        assert_eq!(executed(&[OUT.to_string()]), want);
    }

    #[test]
    fn dumps_read_in_request_order_then_the_exit_dump() {
        let out = out_file("dumps");
        let at = |suffix: &str| PathBuf::from(format!("{}{suffix}", out.display()));
        std::fs::write(at(".2"), "second").unwrap();
        std::fs::write(at(""), "exit").unwrap();
        std::fs::write(at(".10"), "tenth").unwrap();
        std::fs::write(at(".1"), "first").unwrap();
        assert_eq!(
            take_dumps(&out).unwrap(),
            ["first", "second", "tenth", "exit"]
        );
        assert!(!at(".1").exists() && !at("").exists());
    }

    #[test]
    fn a_dump_summary_is_its_total_ir() {
        assert_eq!(summary_ir(OUT), Ok(1052));
        assert!(summary_ir("events: Ir\n").is_err());
    }

    #[test]
    fn executed_functions_span_every_dump() {
        let setup = "ob=/work/bench\nfn=setup\n1 10\n".to_string();
        let window = "ob=/work/bench\nfn=hot\n1 10\n".to_string();
        let names: Vec<_> = executed(&[setup, window])
            .into_iter()
            .map(|(_, f)| f)
            .collect();
        assert_eq!(names, ["hot", "setup"]);
    }
}
