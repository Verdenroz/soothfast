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

/// Every table jumps to the last instruction.
fn prove_in(lines: &[&str]) -> Vec<Proof> {
    let instrs = code(lines);
    let last = instrs.last().map(|(a, _)| *a).unwrap_or(0);
    let span = (0x1000, last + 4);
    prove(&instrs, span, &|tables| {
        tables.iter().map(|t| (*t, vec![last])).collect()
    })
    .proofs
}

const LEA: &str = "lea    0x100d(%rip),%rcx        # 2010 <anon>";
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
        "cwtd",
        "xgetbv",
        "cmpxchg16b (%rdi)",
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
    let proofs = prove(&instrs, (0x1000, 0x1020), &|tables| {
        tables.iter().map(|t| (*t, vec![at(4), at(6)])).collect()
    })
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
