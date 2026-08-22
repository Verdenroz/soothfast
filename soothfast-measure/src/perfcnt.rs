//! Deterministic backend: user-space CPU counters via a hand-rolled
//! `perf_event_open` wrapper (Linux). Per-iteration counts computed by the
//! K vs 2K subtraction trick, which cancels collector/setup overhead exactly.
//!
//! Bare-metal instruction counts are stable to ~1 part in 30M; Docker's
//! default seccomp profile blocks the syscall (probe → EPERM).

use std::mem;

use soothfast_registry::Bencher;

const PERF_TYPE_HARDWARE: u32 = 0;
const HW_CPU_CYCLES: u64 = 0;
const HW_INSTRUCTIONS: u64 = 1;
const HW_CACHE_REFERENCES: u64 = 2;
const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 8;
// Untyped at the call site on purpose: `ioctl` takes a `c_ulong` request on
// glibc and a `c_int` one on musl, so only inference gets both right.
const IOC_RESET: u32 = 0x2403;
const IOC_ENABLE: u32 = 0x2400;
const IOC_DISABLE: u32 = 0x2401;
const FLAG_DISABLED: u64 = 1 << 0;
const FLAG_EXCLUDE_KERNEL: u64 = 1 << 5;
const FLAG_EXCLUDE_HV: u64 = 1 << 6;

/// PERF_ATTR_SIZE_VER0 (64-byte) prefix of `struct perf_event_attr`.
/// The kernel accepts any historically valid `size`; we only need VER0 fields.
#[repr(C)]
#[derive(Clone, Copy)]
struct PerfEventAttr {
    type_: u32,
    size: u32,
    config: u64,
    sample_period_or_freq: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup: u32,
    bp_type: u32,
    config1: u64,
}

struct Counter {
    fd: libc::c_int,
}

impl Counter {
    fn open(config: u64) -> Result<Self, i32> {
        let mut attr: PerfEventAttr = unsafe { mem::zeroed() };
        attr.type_ = PERF_TYPE_HARDWARE;
        attr.size = mem::size_of::<PerfEventAttr>() as u32;
        attr.config = config;
        attr.flags = FLAG_DISABLED | FLAG_EXCLUDE_KERNEL | FLAG_EXCLUDE_HV;
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                &attr as *const _,
                0,  // this process
                -1, // any cpu
                -1, // no group
                PERF_FLAG_FD_CLOEXEC,
            )
        } as libc::c_int;
        if fd < 0 {
            Err(unsafe { *libc::__errno_location() })
        } else {
            Ok(Counter { fd })
        }
    }

    fn count(&self, work: &mut dyn FnMut()) -> u64 {
        unsafe {
            libc::ioctl(self.fd, IOC_RESET as _, 0);
            libc::ioctl(self.fd, IOC_ENABLE as _, 0);
        }
        work();
        unsafe {
            libc::ioctl(self.fd, IOC_DISABLE as _, 0);
        }
        let mut value: u64 = 0;
        let n = unsafe {
            libc::read(
                self.fd,
                &mut value as *mut u64 as *mut libc::c_void,
                mem::size_of::<u64>(),
            )
        };
        assert_eq!(n as usize, mem::size_of::<u64>(), "short read on perf fd");
        value
    }
}

impl Drop for Counter {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

/// Can this environment open an instructions counter on itself?
pub fn probe() -> Result<(), String> {
    Counter::open(HW_INSTRUCTIONS).map(drop).map_err(|errno| {
        let hint = match errno {
            libc::EPERM | libc::EACCES => {
                "blocked (seccomp/paranoid — containers need --security-opt seccomp=unconfined)"
            }
            libc::ENOENT => "event unsupported (no PMU?)",
            libc::ENOSYS => "syscall unavailable",
            _ => "unavailable",
        };
        format!("perf_event_open errno={errno}: {hint}")
    })
}

/// Per-iteration counter values for one item.
#[derive(Debug, Clone, Copy)]
pub struct PerfMeasurement {
    pub instructions: u64,
    pub cycles: u64,
    pub cache_refs: u64,
}

fn count_once(counter: &Counter, runner: &impl Fn(&mut Bencher), iters: u64) -> u64 {
    let mut value = 0u64;
    let mut collector = |body: &mut dyn FnMut()| {
        body();
        value = counter.count(body);
    };
    runner(&mut Bencher::__new(iters, &mut collector));
    value
}

/// Measure per-iteration instructions/cycles/cache-refs (min of 3 rounds).
pub fn measure(runner: impl Fn(&mut Bencher)) -> PerfMeasurement {
    let instr = Counter::open(HW_INSTRUCTIONS).expect("probe passed but open failed");
    let cycles = Counter::open(HW_CPU_CYCLES).ok();
    let cache = Counter::open(HW_CACHE_REFERENCES).ok();

    // Pilot: size K so the counted region dwarfs per-call overhead.
    let pilot = count_once(&instr, &runner, 1).max(1);
    let k = (500_000 / pilot).clamp(1, 1_000_000);

    let mut best = PerfMeasurement {
        instructions: min_of_valid_rounds(&instr, &runner, k).expect(
            "perfcnt: no valid instruction-count round (2K work never counted more than K — \
             body fully const-folded away? route inputs through soothfast::keep)",
        ),
        cycles: 0,
        cache_refs: 0,
    };
    if let Some(c) = &cycles {
        best.cycles = min_of_valid_rounds(c, &runner, k).unwrap_or(0);
    }
    if let Some(c) = &cache {
        best.cache_refs = min_of_valid_rounds(c, &runner, k).unwrap_or(0);
    }
    best
}

/// One K-vs-2K round. A round where the 2K run counted no more events than
/// the K run is provably corrupt (interrupt/migration burst) — reject it
/// rather than min-in a saturated 0, which would poison the baseline and
/// permanently disable gating on this metric downstream.
fn round(c: &Counter, runner: &impl Fn(&mut Bencher), k: u64) -> Option<u64> {
    let low = count_once(c, runner, k);
    let high = count_once(c, runner, 2 * k);
    (high > low).then(|| (high - low) / k)
}

/// Min over 3 valid rounds, retrying corrupt ones (bounded attempts).
fn min_of_valid_rounds(c: &Counter, runner: &impl Fn(&mut Bencher), k: u64) -> Option<u64> {
    let mut vals = Vec::with_capacity(3);
    for _ in 0..10 {
        if let Some(v) = round(c, runner, k) {
            vals.push(v);
            if vals.len() == 3 {
                break;
            }
        }
    }
    vals.into_iter().min()
}

/// Instructions-only measurement, for complexity sweeps.
pub fn measure_instructions(runner: impl Fn(&mut Bencher)) -> u64 {
    let instr = Counter::open(HW_INSTRUCTIONS).expect("probe passed but open failed");
    let pilot = count_once(&instr, &runner, 1).max(1);
    let k = (500_000 / pilot).clamp(1, 1_000_000);
    min_of_valid_rounds(&instr, &runner, k).expect(
        "perfcnt: no valid instruction-count round (2K work never counted more than K — \
         body fully const-folded away? route inputs through soothfast::keep)",
    )
}
