use super::*;

/// Instruction `i` sits at `0x1000 + 4 * i`.
fn at(i: u64) -> u64 {
    0x1000 + 4 * i
}

fn code(lines: &[&str]) -> Vec<(u64, String)> {
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| (at(i as u64), l.to_string()))
        .collect()
}

/// Where jumps may leave for, and what kind of function this is.
struct World {
    /// Addresses that are the entry of another function.
    entries: &'static [u64],
    slots_leave: bool,
    rust: bool,
}

impl Exits for World {
    fn branch_leaves(&self, target: u64) -> bool {
        self.entries.contains(&target)
    }

    fn slot_leaves(&self, _: u64) -> bool {
        self.slots_leave
    }

    fn rust(&self) -> bool {
        self.rust
    }

    fn data_table(&self, table: u64) -> Option<(u64, u64)> {
        Some((table, table + 0x10))
    }
}

const RUST: World = World {
    entries: &[0x2000],
    slots_leave: true,
    rust: true,
};

/// Every table jumps to the last instruction.
fn prove_in(lines: &[&str]) -> Vec<Proof> {
    prove_world(lines, &RUST)
}

fn prove_world(lines: &[&str], world: &World) -> Vec<Proof> {
    let instrs = code(lines);
    let last = instrs.last().map(|(a, _)| *a).unwrap_or(0);
    let span = (0x1000, last + 4);
    prove(
        &instrs,
        span,
        &|tables| tables.iter().map(|t| (*t, vec![last])).collect(),
        &|_| HashMap::new(),
        world,
    )
    .proofs
}

const LEA: &str = "lea    0x100d(%rip),%rcx        # 2010 <anon>";
const LEA_SI: &str = "lea    0x100d(%rip),%rsi        # 2010 <anon>";
const LEA_DX: &str = "lea    0x100d(%rip),%rdx        # 2010 <anon>";
const LOAD: &str = "movslq (%rcx,%rax,4),%rax";
const ADD: &str = "add    %rcx,%rax";
const JMP: &str = "jmp    *%rax";
const RET: &str = "ret";

#[test]
fn the_exact_dataflow_proves_a_table() {
    assert_eq!(
        prove_in(&[LEA, LOAD, ADD, JMP, RET]),
        [Proof {
            lea: 0x1000,
            table: 0x2010
        }]
    );
}

#[test]
fn either_add_order_and_a_notrack_jump_prove_it() {
    let swapped = [
        "lea    0x100d(%rip),%rdx        # 2010 <anon>",
        "movslq (%rdx,%rcx,4),%rax",
        "add    %rax,%rdx",
        "notrack jmp *%rdx",
        RET,
    ];
    assert_eq!(prove_in(&swapped).len(), 1);
}

#[test]
fn a_write_to_the_base_register_breaks_the_proof() {
    assert!(prove_in(&[LEA, "mov    %rbx,%rcx", LOAD, ADD, JMP, RET]).is_empty());
}

#[test]
fn a_write_to_the_loaded_or_summed_register_breaks_the_proof() {
    assert!(prove_in(&[LEA, LOAD, "xor    %eax,%eax", ADD, JMP, RET]).is_empty());
    assert!(prove_in(&[LEA, LOAD, ADD, "inc    %rax", JMP, RET]).is_empty());
}

#[test]
fn a_jump_away_or_an_exit_before_the_jump_breaks_the_proof() {
    assert!(prove_in(&[LEA, "jmp    1018 <f+0x18>", LOAD, ADD, JMP, RET]).is_empty());
    assert!(prove_in(&[LEA, RET, LOAD, ADD, JMP, RET]).is_empty());
}

#[test]
fn an_implicit_writer_breaks_the_proof() {
    for odd in [
        "rdtsc",
        "lahf",
        "xgetbv",
        "cpuid",
        "syscall",
        "xchg   %rbx,%rcx",
        "lock xadd %rcx,(%rdi)",
        "cmpxchg %rbx,%rcx",
    ] {
        assert!(
            prove_in(&[LEA, odd, LOAD, ADD, JMP, RET]).is_empty(),
            "{odd}"
        );
    }
    assert_eq!(
        prove_in(&[LEA, "nop", "pause", LOAD, ADD, JMP, RET]).len(),
        1
    );
}

#[test]
fn a_string_op_writes_its_index_registers() {
    for odd in ["cmpsb  %es:(%rdi),%ds:(%rsi)", "lodsq  %ds:(%rsi),%rax"] {
        let lines = [
            LEA_SI,
            odd,
            "movslq (%rsi,%rax,4),%rax",
            "add    %rsi,%rax",
            JMP,
            RET,
        ];
        assert!(prove_in(&lines).is_empty(), "{odd}");
    }
}

#[test]
fn a_widening_multiply_writes_rdx() {
    let lines = [
        LEA_DX,
        "imulq  0x8(%rsp)",
        "movslq (%rdx,%rax,4),%rax",
        "add    %rdx,%rax",
        JMP,
        RET,
    ];
    assert!(prove_in(&lines).is_empty());
}

#[test]
fn a_tail_call_through_a_got_slot_leaves_the_function() {
    let lines = [
        LEA,
        LOAD,
        ADD,
        JMP,
        "jmp    *0x650abb(%rip)        # 12651d8 <_DYNAMIC+0x210>",
        RET,
    ];
    assert_eq!(prove_in(&lines).len(), 1);
    let through_register = [LEA, LOAD, ADD, JMP, "jmp    *%rdx", RET];
    assert!(prove_in(&through_register).is_empty());
}

#[test]
fn an_implicit_writer_of_other_registers_keeps_the_proof() {
    for ok in [
        "cwtd",
        "cqto",
        "cltq",
        "mul    %rdx",
        "divq   0x8(%rsp)",
        "imul   %rsi",
        "imulq  0x8(%rsp)",
        "cmpxchg16b (%rdi)",
        "movsq  (%rsi),(%rdi)",
    ] {
        assert_eq!(prove_in(&[LEA, ok, LOAD, ADD, JMP, RET]).len(), 1, "{ok}");
    }
}

#[test]
fn an_exchange_with_memory_writes_only_its_register_operands() {
    for ok in [
        "lock cmpxchg %rbx,(%rdi)",
        "xchg   %eax,(%rdi)",
        "lock xadd %rbx,(%rdi)",
        "xchg   %rbx,%rdx",
    ] {
        assert_eq!(prove_in(&[LEA, ok, LOAD, ADD, JMP, RET]).len(), 1, "{ok}");
    }
}

#[test]
fn a_load_that_is_not_scaled_by_four_is_no_proof() {
    assert!(prove_in(&[LEA, "movslq (%rcx,%rax,8),%rax", ADD, JMP, RET]).is_empty());
}

#[test]
fn a_branch_into_the_block_breaks_the_proof() {
    // The conditional jump targets the add, so another path reaches it.
    assert!(prove_in(&[LEA, "jne    100c <f+0xc>", LOAD, ADD, JMP, RET]).is_empty());
    // Targeting the final ret leaves the block alone.
    assert_eq!(
        prove_in(&[LEA, "jne    1014 <f+0x14>", LOAD, ADD, JMP, RET]).len(),
        1
    );
}

#[test]
fn a_base_hoisted_across_a_call_in_a_callee_saved_register_proves() {
    let lea = "lea    0x100d(%rip),%r14        # 2010 <anon>";
    let load = "movslq (%r14,%rax,4),%rax";
    let add = "add    %r14,%rax";
    let hoisted = [lea, "call   2000 <g>", load, add, JMP, RET];
    assert_eq!(prove_in(&hoisted).len(), 1);
}

#[test]
fn the_same_across_a_call_in_a_caller_saved_register_does_not() {
    assert!(prove_in(&[LEA, "call   2000 <g>", LOAD, ADD, JMP, RET]).is_empty());
}

#[test]
fn a_path_that_redefines_the_base_before_the_load_does_not_prove() {
    let lea = "lea    0x100d(%rip),%r14        # 2010 <anon>";
    let load = "movslq (%r14,%rax,4),%rax";
    let add = "add    %r14,%rax";
    let lines = [
        lea,
        "jne    1010 <f+0x10>",
        "mov    %rbx,%r14",
        "nop",
        load,
        add,
        JMP,
        RET,
    ];
    assert!(prove_in(&lines).is_empty());
}

#[test]
fn a_loop_back_edge_that_keeps_the_lea_dominant_proves() {
    let lea = "lea    0x100d(%rip),%r14        # 2010 <anon>";
    let load = "movslq (%r14,%rax,4),%rax";
    let add = "add    %r14,%rax";
    let lines = [
        lea,
        load,
        add,
        JMP,
        "dec    %rdi",
        "jmp    1004 <f+0x4>",
        RET,
    ];
    let instrs = code(&lines);
    let proofs = prove(
        &instrs,
        (0x1000, 0x1020),
        &|tables| tables.iter().map(|t| (*t, vec![at(4), at(6)])).collect(),
        &|_| HashMap::new(),
        &RUST,
    )
    .proofs;
    assert_eq!(proofs.len(), 1);
}

#[test]
fn an_unknown_indirect_jump_elsewhere_disables_every_proof() {
    assert!(prove_in(&[LEA, LOAD, ADD, JMP, "jmp    *%rdx", RET]).is_empty());
    assert!(prove_in(&[LEA, LOAD, ADD, JMP, "call   *%rdx", RET]).len() == 1);
}

#[test]
fn a_block_nothing_reaches_that_writes_the_base_breaks_the_proof() {
    let plain = [LEA, "jmp    100c <f+0xc>", "ud2", LOAD, ADD, JMP, RET];
    assert_eq!(prove_in(&plain).len(), 1);
    let landing_pad = [
        LEA,
        "jmp    1014 <f+0x14>",
        "ud2",
        "mov    %rbx,%rcx",
        "jmp    1014 <f+0x14>",
        LOAD,
        ADD,
        JMP,
        RET,
    ];
    assert!(prove_in(&landing_pad).is_empty());
}

#[test]
fn padding_between_blocks_is_not_an_entry() {
    let padded = [
        LEA,
        "jmp    1014 <f+0x14>",
        "ud2",
        "nopw   0x0(%rax,%rax,1)",
        "nop",
        LOAD,
        ADD,
        JMP,
        RET,
    ];
    assert_eq!(prove_in(&padded).len(), 1);
    let pad_then_pad = [
        LEA,
        "jmp    1014 <f+0x14>",
        "ud2",
        "nop",
        "mov    %rbx,%rcx",
        LOAD,
        ADD,
        JMP,
        RET,
    ];
    assert!(prove_in(&pad_then_pad).is_empty());
}

const LEA15: &str = "lea    0x100d(%rip),%r15        # 2010 <anon>";
const LOAD15: &str = "movslq (%r15,%rax,4),%rax";
const ADD15: &str = "add    %r15,%rax";

#[test]
fn a_landing_pad_inherits_callee_saved_registers_from_the_calls() {
    let lines = [
        LEA15,
        "call   2000 <g>",
        "jmp    1018 <f+0x18>",
        "ud2",
        "mov    %rax,%r14",
        "jmp    1018 <f+0x18>",
        LOAD15,
        ADD15,
        JMP,
        RET,
    ];
    assert_eq!(prove_in(&lines).len(), 1);
}

fn unwind_resume_before_a_case_block(callee: &str) -> Vec<Proof> {
    prove_in(&[
        LEA15,
        "call   2000 <g>",
        "jmp    1018 <f+0x18>",
        "ud2",
        "mov    %rbx,%r15",
        callee,
        LOAD15,
        ADD15,
        JMP,
        RET,
    ])
}

#[test]
fn a_pad_ending_in_unwind_resume_does_not_flow_into_the_next_block() {
    for callee in [
        "call   3000 <_Unwind_Resume@plt>",
        "call   3000 <_Unwind_Resume@GCC_3.0>",
        "call   *0x20(%rip)        # 3000 <_Unwind_Resume@GCC_3.0>",
    ] {
        assert_eq!(
            unwind_resume_before_a_case_block(callee).len(),
            1,
            "{callee}"
        );
    }
}

#[test]
fn a_pad_ending_in_another_call_flows_into_the_next_block() {
    for callee in [
        "call   3000 <foo>",
        "call   3000 <_Unwind_Resume_or_Not@plt>",
    ] {
        assert!(
            unwind_resume_before_a_case_block(callee).is_empty(),
            "{callee}"
        );
    }
}

#[test]
fn a_call_before_the_lea_leaves_the_register_undefined_in_the_pad() {
    let lines = [
        "call   2000 <g>",
        LEA15,
        "call   2000 <g>",
        "jmp    101c <f+0x1c>",
        "ud2",
        "mov    %rax,%r14",
        "jmp    101c <f+0x1c>",
        LOAD15,
        ADD15,
        JMP,
        RET,
    ];
    assert!(prove_in(&lines).is_empty());
}

#[test]
fn a_caller_saved_base_is_unknown_in_a_landing_pad() {
    let lines = [
        LEA,
        "call   2000 <g>",
        "jmp    1018 <f+0x18>",
        "ud2",
        "mov    %rax,%r14",
        "jmp    1018 <f+0x18>",
        LOAD,
        ADD,
        JMP,
        RET,
    ];
    assert!(prove_in(&lines).is_empty());
}

#[test]
fn registers_alias_across_widths() {
    assert_eq!(family("%eax"), family("%rax"));
    assert_eq!(family("%r10d"), family("%r10"));
    assert_ne!(family("%r10"), family("%r11"));
}

#[test]
fn unwind_resume_does_not_unwind_into_another_pad() {
    let lines = [
        LEA15,
        "call   2000 <g>",
        "jmp    1020 <f+0x20>",
        "ud2",
        "mov    %rbx,%r15",
        "call   3000 <_Unwind_Resume@plt>",
        "mov    %rax,%r14",
        "jmp    1020 <f+0x20>",
        LOAD15,
        ADD15,
        JMP,
        RET,
    ];
    assert_eq!(prove_in(&lines).len(), 1);
}

#[test]
fn a_branch_out_of_the_function_is_an_exit_only_to_the_entry_of_another() {
    let entry = "jne    2000 <_RNvNtCsg0jXFtQWPuf_9scry_core4repo5other>";
    assert_eq!(prove_in(&[LEA, entry, LOAD, ADD, JMP, RET]).len(), 1);
    let unconditional = "jmp    2000 <_RNvNtCsg0jXFtQWPuf_9scry_core4repo5other>";
    assert_eq!(
        prove_in(&[LEA, LOAD, ADD, JMP, unconditional, RET]).len(),
        1
    );
    for target in ["2010 <other+0x10>", "2100 <sqlite3VdbeExec.cold>", "2200"] {
        for jump in ["jne", "jmp"] {
            let branch = format!("{jump}    {target}");
            assert!(
                prove_in(&[LEA, &branch, LOAD, ADD, JMP, RET]).is_empty(),
                "{branch}"
            );
        }
    }
}

#[test]
fn a_tail_call_through_a_slot_is_an_exit_only_when_the_slot_leaves() {
    let slot = "jmp    *0x650abb(%rip)        # 12651d8 <_DYNAMIC+0x210>";
    let lines = [LEA, LOAD, ADD, JMP, slot, RET];
    assert_eq!(prove_in(&lines).len(), 1);
    let stays = World {
        slots_leave: false,
        ..RUST
    };
    assert!(prove_world(&lines, &stays).is_empty());
    let no_address = "jmp    *0x650abb(%rip)";
    assert!(prove_in(&[LEA, LOAD, ADD, JMP, no_address, RET]).is_empty());
}

#[test]
fn a_tail_call_through_a_pointer_in_memory_leaves_a_rust_function() {
    let vtable = "jmp    *0x17(%rax)";
    assert_eq!(prove_in(&[LEA, LOAD, ADD, JMP, vtable, RET]).len(), 1);
    assert_eq!(
        prove_in(&[LEA, LOAD, ADD, JMP, "jmp    *(%rdx)", RET]).len(),
        1
    );
    let indexed = "jmp    *0x0(,%rax,8)";
    assert!(prove_in(&[LEA, LOAD, ADD, JMP, indexed, RET]).is_empty());
}

fn exit_through_rdx(first: &str, world: &World) -> Vec<Proof> {
    prove_world(
        &[
            LEA,
            first,
            "je     1018 <f+0x18>",
            LOAD,
            ADD,
            JMP,
            "jmp    *%rdx",
        ],
        world,
    )
}

#[test]
fn a_tail_call_through_a_register_needs_a_pointer_loaded_on_every_path() {
    let load = "mov    0x18(%rdx),%rdx";
    assert_eq!(exit_through_rdx(load, &RUST).len(), 1);
    assert!(exit_through_rdx("mov    %rbx,%rdx", &RUST).is_empty());
    assert!(exit_through_rdx("mov    0x0(,%rax,8),%rdx", &RUST).is_empty());
    assert!(exit_through_rdx("add    0x18(%rdx),%rdx", &RUST).is_empty());
    let skipped = [
        LEA,
        "je     1018 <f+0x18>",
        load,
        LOAD,
        ADD,
        JMP,
        "jmp    *%rdx",
    ];
    assert!(prove_in(&skipped).is_empty());
}

#[test]
fn a_loaded_pointer_is_no_exit_from_a_function_that_may_use_computed_goto() {
    let c = World {
        rust: false,
        ..RUST
    };
    let load = "mov    0x18(%rdx),%rdx";
    assert!(exit_through_rdx(load, &c).is_empty());
    let memory = [LEA, LOAD, ADD, JMP, "jmp    *0x17(%rax)", RET];
    assert!(prove_world(&memory, &c).is_empty());
    let tail = [LEA, LOAD, ADD, JMP, "jmp    2000 <other>", RET];
    assert_eq!(prove_world(&tail, &c).len(), 1);
}

#[test]
fn two_exits_through_one_register_are_judged_together() {
    let lines = [
        LEA,
        "mov    0x18(%rdx),%rdx",
        "je     1018 <f+0x18>",
        LOAD,
        ADD,
        JMP,
        "jmp    *%rdx",
        "jmp    *%rdx",
    ];
    assert_eq!(prove_in(&lines).len(), 1);
}

/// An image whose table input is read-only (or not, with `None`), and where
/// every table of offsets to data names the address `0x3000`.
struct Data {
    input: Option<(u64, u64)>,
}

impl Exits for Data {
    fn branch_leaves(&self, _: u64) -> bool {
        false
    }

    fn slot_leaves(&self, _: u64) -> bool {
        true
    }

    fn rust(&self) -> bool {
        true
    }

    fn data_table(&self, _: u64) -> Option<(u64, u64)> {
        self.input
    }
}

fn prove_data_in(lines: &[&str], input: Option<(u64, u64)>) -> Proven {
    let instrs = code(lines);
    let last = instrs.last().map(|(a, _)| *a).unwrap_or(0);
    prove(
        &instrs,
        (0x1000, last + 4),
        &|tables| tables.iter().map(|t| (*t, vec![last])).collect(),
        &|tables| tables.iter().map(|t| (*t, vec![0x3000])).collect(),
        &Data { input },
    )
}

fn prove_data(lines: &[&str]) -> Proven {
    prove_data_in(lines, Some((0x2010, 0x2020)))
}

const LOAD_SI: &str = "movslq (%rsi,%rcx,4),%rax";
const ADD_SI: &str = "add    %rsi,%rax";

/// `StandardHeader::as_str`.
const AS_STR: [&str; 7] = [
    "movzbl %dil,%ecx",
    "lea    0x100d(%rip),%rax        # 2000 <lengths>",
    "mov    (%rax,%rcx,8),%rdx",
    LEA_SI,
    LOAD_SI,
    ADD_SI,
    RET,
];

/// An enum's `Debug::fmt`.
const KIND_FMT: [&str; 10] = [
    "mov    %rsi,%rax",
    "mov    (%rdi),%rcx",
    "movzbl (%rcx),%ecx",
    "lea    -0xec5528(%rip),%rdx        # 2000 <anon.a+0x18d0>",
    "mov    (%rdx,%rcx,8),%rdx",
    "lea    -0xec54db(%rip),%rdi        # 2010 <anon.a+0x1928>",
    "movslq (%rdi,%rcx,4),%rsi",
    "add    %rdi,%rsi",
    "mov    %rax,%rdi",
    "jmp    *0x156545(%rip)        # 4000 <_DYNAMIC+0xa40>",
];

fn proven_once(proven: &Proven, lea: u64) {
    assert!(proven.proofs.is_empty());
    assert_eq!(proven.pointers, [Proof { lea, table: 0x2010 }]);
    assert_eq!(proven.targets[&0x2010], [0x3000]);
}

#[test]
fn the_base_overwritten_before_a_tail_call_proves_a_table_of_offsets() {
    proven_once(&prove_data(&KIND_FMT), at(5));
}

#[test]
fn a_return_ends_the_life_of_a_base_that_is_not_a_return_register() {
    proven_once(&prove_data(&AS_STR), at(3));
}

#[test]
fn a_return_reads_the_return_registers() {
    let lines = [
        "lea    0x100d(%rip),%rax        # 2010 <anon>",
        "movslq (%rax,%rcx,4),%rdx",
        "add    %rax,%rdx",
        RET,
    ];
    assert!(prove_data(&lines).pointers.is_empty());
}

#[test]
fn a_tail_jump_with_the_base_still_in_place_is_no_proof() {
    let lines = [
        "lea    0x100d(%rip),%rcx        # 2010 <anon>",
        "movslq (%rcx,%r14,4),%rax",
        "add    %rcx,%rax",
        "mov    %rsi,%rdi",
        "mov    %rax,%rsi",
        "add    $0x28,%rsp",
        "jmp    *0x156545(%rip)        # 4000 <_DYNAMIC+0xa40>",
    ];
    assert!(prove_data(&lines).pointers.is_empty());
}

#[test]
fn a_second_reference_to_the_table_is_no_proof() {
    let mut lines = AS_STR.to_vec();
    lines.insert(1, "lea    0x100d(%rip),%rdx        # 2014 <anon+4>");
    assert!(prove_data(&lines).pointers.is_empty());
}

#[test]
fn a_table_in_writable_or_sealed_data_is_no_proof() {
    assert!(prove_data_in(&AS_STR, None).pointers.is_empty());
}

#[test]
fn a_write_to_a_register_of_the_read_in_between_is_no_proof() {
    for between in ["mov    $0x1,%ecx", "mov    %rsi,%rdx", "mov    %rax,%rdx"] {
        let mut before_load = AS_STR.to_vec();
        before_load.insert(4, between);
        assert!(prove_data(&before_load).pointers.is_empty(), "{between}");
        let mut before_add = AS_STR.to_vec();
        before_add.insert(5, between);
        assert!(prove_data(&before_add).pointers.is_empty(), "{between}");
    }
    let mut elsewhere = AS_STR.to_vec();
    elsewhere.insert(5, "mov    %r8,%r9");
    proven_once(&prove_data(&elsewhere), at(3));
}

#[test]
fn a_read_of_the_base_after_the_add_is_no_proof() {
    let lines = [
        LEA_SI,
        LOAD_SI,
        ADD_SI,
        "mov    (%rsi),%rdx",
        "mov    %rax,%rsi",
        RET,
    ];
    assert!(prove_data(&lines).pointers.is_empty());
}

#[test]
fn a_call_or_a_partial_write_does_not_end_the_base() {
    for after in ["call   2000 <g>", "mov    %al,%sil", "nop"] {
        let lines = [LEA_SI, LOAD_SI, ADD_SI, after, "mov    %rax,%rsi", RET];
        assert!(prove_data(&lines).pointers.is_empty(), "{after}");
    }
    let whole = [LEA_SI, LOAD_SI, ADD_SI, "mov    %eax,%esi", RET];
    proven_once(&prove_data(&whole), at(0));
}

#[test]
fn code_that_ends_before_the_base_does_is_no_proof() {
    assert!(prove_data(&[LEA_SI, LOAD_SI, ADD_SI]).pointers.is_empty());
    let mut long = vec![LEA_SI, LOAD_SI, ADD_SI];
    long.extend(["mov    %rax,0x8(%rsp)"; 70]);
    long.extend(["mov    %rax,%rsi", RET]);
    assert!(prove_data(&long).pointers.is_empty());
}

#[test]
fn an_add_into_the_base_or_a_second_load_is_no_proof() {
    let into_base = [LEA_SI, LOAD_SI, "add    %rax,%rsi", "mov    $0x0,%esi", RET];
    assert!(prove_data(&into_base).pointers.is_empty());
    let differences = [
        LEA_SI,
        LOAD_SI,
        "movslq 0x4(%rsi,%rcx,4),%rdx",
        ADD_SI,
        "sub    %rax,%rdx",
        RET,
    ];
    assert!(prove_data(&differences).pointers.is_empty());
}

#[test]
fn a_load_into_the_base_register_is_no_table_read() {
    let load = "movslq (%rcx,%rax,4),%rcx";
    let add = "add    %rdx,%rcx";
    assert!(prove_in(&[LEA, load, add, "jmp    *%rcx", RET]).is_empty());
    let proven = prove_data(&[LEA, load, add, RET]);
    assert!(proven.pointers.is_empty() && proven.proofs.is_empty());
}

#[test]
fn an_add_of_a_third_register_is_no_table_read() {
    assert!(prove_in(&[LEA, LOAD, "add    %rdx,%rax", JMP, RET]).is_empty());
    let proven = prove_data(&[LEA, LOAD, "add    %rdx,%rax", RET]);
    assert!(proven.pointers.is_empty());
}

#[test]
fn a_table_read_as_offsets_and_as_jumps_is_a_jump_table() {
    let proven = prove_data(&[LEA, LOAD, ADD, JMP, LEA, LOAD, ADD, RET]);
    assert!(proven.pointers.is_empty());
    assert_eq!(proven.proofs.len(), 2);
}

#[test]
fn a_jump_table_and_a_name_table_through_one_register_are_both_proven() {
    let jump_then_names = [
        LEA,
        LOAD,
        ADD,
        JMP,
        "lea    0x100d(%rip),%rcx        # 2020 <names>",
        LOAD,
        ADD,
        RET,
    ];
    let instrs = code(&jump_then_names);
    let proven = prove(
        &instrs,
        (0x1000, at(8)),
        &|tables| {
            tables
                .iter()
                .filter(|t| **t == 0x2010)
                .map(|t| (*t, vec![at(4)]))
                .collect()
        },
        &|tables| {
            tables
                .iter()
                .filter(|t| **t == 0x2020)
                .map(|t| (*t, vec![0x3000]))
                .collect()
        },
        &RUST,
    );
    assert_eq!(proven.proofs.len(), 1);
    assert_eq!(proven.proofs[0].table, 0x2010);
    assert_eq!(proven.pointers.len(), 1);
    assert_eq!(proven.pointers[0].table, 0x2020);
}
