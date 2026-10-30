//! Callgrind client requests, hand-rolled so the runner takes no dependency
//! for them. A request is a fixed no-op instruction sequence that valgrind's
//! JIT recognises; run natively it changes nothing.

/// Whether [`dump_stats`] reaches callgrind on this target.
pub const SUPPORTED: bool = cfg!(any(target_arch = "x86_64", target_arch = "aarch64"));

/// `CALLGRIND_DUMP_STATS`: `VG_USERREQ_TOOL_BASE('C', 'T')`.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const DUMP_STATS: u64 = 0x4354_0000;

/// Write the counters accumulated so far to a new dump file and zero them.
/// A no-op outside valgrind.
#[cfg(target_arch = "x86_64")]
pub fn dump_stats() {
    let args = [DUMP_STATS, 0, 0, 0, 0, 0u64];
    // SAFETY: the rotates sum to a multiple of 64 and `xchg rbx, rbx` swaps a
    // register with itself, so natively this changes only the flags. Under
    // valgrind the sequence is a request whose argument block is `args`.
    unsafe {
        std::arch::asm!(
            "rol rdi, 3",
            "rol rdi, 13",
            "rol rdi, 61",
            "rol rdi, 51",
            "xchg rbx, rbx",
            inout("rdx") 0u64 => _,
            in("rax") args.as_ptr(),
            options(nostack),
        );
    }
}

/// Write the counters accumulated so far to a new dump file and zero them.
/// A no-op outside valgrind.
#[cfg(target_arch = "aarch64")]
pub fn dump_stats() {
    let args = [DUMP_STATS, 0, 0, 0, 0, 0u64];
    // SAFETY: the rotates sum to a multiple of 64 and `orr x10, x10, x10`
    // copies a register onto itself, so natively this changes nothing. Under
    // valgrind the sequence is a request whose argument block is `args`.
    unsafe {
        std::arch::asm!(
            "ror x12, x12, #3",
            "ror x12, x12, #13",
            "ror x12, x12, #51",
            "ror x12, x12, #61",
            "orr x10, x10, x10",
            inout("x3") 0u64 => _,
            in("x4") args.as_ptr(),
            options(nostack),
        );
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub fn dump_stats() {}

#[cfg(test)]
mod tests {
    use super::dump_stats;

    #[test]
    fn a_dump_request_outside_valgrind_does_nothing() {
        dump_stats();
        dump_stats();
    }
}
