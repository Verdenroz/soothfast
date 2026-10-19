//! Jump tables proven by the instructions that use them, x86-64 AT&T syntax.
//!
//! A table of 32-bit code offsets changes whenever code moves, so its bytes
//! say nothing stable. Where one function reads a table through exactly this
//! dataflow, the words are offsets from the table's base to places in that
//! function:
//!
//! ```text
//! lea    T(%rip),%rA
//! movslq (%rA,%rI,4),%rB
//! add    %rA,%rB          (either operand order)
//! jmp    *%rB
//! ```
//!
//! The base register may be loaded earlier in the function, provided every
//! path to the load defines it the same way (see [`prove`]). Any other shape
//! is no proof, and the caller hashes the bytes.

use std::collections::{HashMap, HashSet};

/// A proven table base: the `lea` that takes it, and the address it takes.
#[derive(Debug, PartialEq)]
pub struct Proof {
    pub lea: u64,
    pub table: u64,
}

/// How far past a `lea` the rest of the pattern may sit.
const WINDOW: usize = 32;

struct Ins<'a> {
    mnemonic: &'a str,
    operands: Vec<&'a str>,
    comment: Option<&'a str>,
}

const PREFIXES: &[&str] = &["notrack", "bnd", "rex", "data16", "cs", "ds"];

fn parse(body: &str) -> Option<Ins<'_>> {
    let (code, comment) = match body.split_once("# ") {
        Some((code, comment)) => (code, Some(comment)),
        None => (body, None),
    };
    let mut rest = code.trim();
    loop {
        let (word, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if !PREFIXES.contains(&word) {
            return Some(Ins {
                mnemonic: word,
                operands: split_operands(tail.trim()),
                comment,
            });
        }
        if tail.is_empty() {
            return None;
        }
        rest = tail.trim_start();
    }
}

/// Split on the commas outside parentheses.
fn split_operands(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0, 0);
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    if !s.is_empty() {
        parts.push(s[start..].trim());
    }
    parts
}

/// The register a name belongs to, whatever its width.
fn family(reg: &str) -> Option<String> {
    let name = reg.trim().strip_prefix('%')?;
    if let Some(n) = name
        .strip_prefix('r')
        .filter(|n| n.starts_with(|c: char| c.is_ascii_digit()))
    {
        let digits: String = n.chars().take_while(char::is_ascii_digit).collect();
        return Some(format!("r{digits}"));
    }
    let base = match name {
        "rax" | "eax" | "ax" | "al" | "ah" => "a",
        "rbx" | "ebx" | "bx" | "bl" | "bh" => "b",
        "rcx" | "ecx" | "cx" | "cl" | "ch" => "c",
        "rdx" | "edx" | "dx" | "dl" | "dh" => "d",
        "rsi" | "esi" | "si" | "sil" => "si",
        "rdi" | "edi" | "di" | "dil" => "di",
        "rbp" | "ebp" | "bp" | "bpl" => "bp",
        "rsp" | "esp" | "sp" | "spl" => "sp",
        _ => return None,
    };
    Some(base.to_string())
}

enum Writes {
    Nothing,
    Reg(String),
    /// Anything may change, as at a call or an implicit-operand instruction.
    Any,
}

fn writes(ins: &Ins) -> Writes {
    let m = ins.mnemonic;
    let starts = |names: &[&str]| names.iter().any(|n| m.starts_with(n));
    let is = |names: &[&str]| names.contains(&m);
    let one_operand_imul = m == "imul" && ins.operands.len() == 1;
    if starts(&[
        "call", "rep", "loop", "cmpxchg", "xchg", "xadd", "cpuid", "syscall",
    ]) || is(&[
        "mul", "mulb", "mulw", "mull", "mulq", "div", "divb", "divw", "divl", "divq",
    ]) || is(&[
        "idiv", "idivb", "idivw", "idivl", "idivq", "cltd", "cqto", "cltq", "cwtl", "leave",
    ]) || is(&[
        "movsb", "movsw", "movsl", "movsq", "stosb", "stosw", "stosl", "stosq",
    ]) || one_operand_imul
    {
        return Writes::Any;
    }
    if m.starts_with('j')
        || starts(&[
            "cmp", "test", "push", "nop", "endbr", "ret", "ud2", "int3", "hlt", "prefetch",
        ])
        || is(&[
            "pause", "lfence", "mfence", "sfence", "cld", "std", "clc", "stc", "cmc",
        ])
    {
        return Writes::Nothing;
    }
    if ins.operands.is_empty() {
        return Writes::Any;
    }
    let dest = if starts(&["pop", "inc", "dec", "neg", "not", "set", "bswap"]) {
        ins.operands.first()
    } else {
        ins.operands.last()
    };
    match dest.and_then(|d| family(d)) {
        Some(f) => Writes::Reg(f),
        None => Writes::Nothing,
    }
}

fn disturbs(ins: &Ins, regs: &[&str]) -> bool {
    match writes(ins) {
        Writes::Nothing => false,
        Writes::Any => true,
        Writes::Reg(f) => regs.contains(&f.as_str()),
    }
}

/// The `(%rA,%rI,4)` base register of a memory operand.
fn scaled_base(operand: &str) -> Option<String> {
    let inner = operand.strip_suffix(",4)")?.rsplit_once('(')?.1;
    let (base, _index) = inner.split_once(',')?;
    family(base)
}

fn direct_target(ins: &Ins) -> Option<u64> {
    let m = ins.mnemonic;
    (m.starts_with('j') || m.starts_with("loop"))
        .then(|| u64::from_str_radix(ins.operands.first()?.split_whitespace().next()?, 16).ok())
        .flatten()
}

fn is_branch(ins: &Ins) -> bool {
    ins.mnemonic.starts_with('j') || ins.mnemonic.starts_with("loop")
}

/// A nop, as compilers pad between blocks.
fn is_padding(ins: &Ins) -> bool {
    ins.mnemonic.starts_with("nop") || (ins.mnemonic == "xchg" && ins.operands == ["%ax", "%ax"])
}

fn ends_block(ins: &Ins) -> bool {
    is_branch(ins) || matches!(ins.mnemonic, "ret" | "retq" | "ud2" | "hlt" | "int3")
}

const CALLER_SAVED: &[&str] = &["a", "c", "d", "si", "di", "r8", "r9", "r10", "r11"];

/// Where each candidate table jumps to, for those whose words decode.
pub type Decoder<'a> = dyn Fn(&[u64]) -> HashMap<u64, Vec<u64>> + 'a;

/// A table read as `movslq (%rA,%rI,4),%rB; add; jmp *%rD`, from one `lea`.
struct Candidate {
    table: u64,
    base: String,
    load: usize,
    jmp: usize,
}

/// Every table `instrs` (one function, in order, spanning `span`) reads in
/// the proven way.
///
/// `decode` takes candidate table addresses and returns the places in the
/// function each one jumps to, for those whose words decode. The proof is a
/// control-flow graph of the function, in which a table jump has an edge to
/// each place its table names:
///
/// - the load, add and `jmp *` sit in one basic block, with no write between
///   to the registers that carry the word or the sum;
/// - every definition of the base register that reaches the load is a `lea`
///   of the same table, along every path from the function's entry. A call
///   keeps the callee-saved registers and clobbers the rest; an instruction
///   that may write anything clobbers all.
///
/// The graph must be complete: every indirect jump is a proven table jump,
/// and every direct branch lands on an instruction of the function, or
/// leaves it by an unconditional tail jump. Otherwise nothing is proven. A
/// candidate that fails is dropped, and the rest are checked again against
/// the graph without it.
pub fn prove(instrs: &[(u64, String)], span: (u64, u64), decode: &Decoder) -> Vec<Proof> {
    let Some(parsed) = instrs
        .iter()
        .map(|(addr, body)| Some((*addr, parse(body)?)))
        .collect::<Option<Vec<(u64, Ins)>>>()
    else {
        return Vec::new();
    };
    let at: HashMap<u64, usize> = parsed
        .iter()
        .enumerate()
        .map(|(i, (a, _))| (*a, i))
        .collect();
    let leas = rip_leas(&parsed);
    let mut active = candidates(&parsed, &leas);
    loop {
        let mut tables: Vec<u64> = active.iter().map(|c| c.table).collect();
        tables.sort_unstable();
        tables.dedup();
        let targets = decode(&tables);
        active.retain(|c| targets.contains_key(&c.table));
        if active.is_empty() {
            return Vec::new();
        }
        let Some(graph) = Graph::build(&parsed, &at, span, &active, &targets) else {
            return Vec::new();
        };
        let before = active.len();
        active.retain(|c| graph.reads_table(&parsed, c, &leas));
        if active.len() == before {
            let proven: HashSet<u64> = active.iter().map(|c| c.table).collect();
            return leas
                .iter()
                .filter(|l| proven.contains(&l.table))
                .map(|l| Proof {
                    lea: parsed[l.at].0,
                    table: l.table,
                })
                .collect();
        }
    }
}

struct Lea {
    at: usize,
    table: u64,
    dest: String,
}

fn rip_leas(parsed: &[(u64, Ins)]) -> Vec<Lea> {
    parsed
        .iter()
        .enumerate()
        .filter(|(_, (_, i))| i.mnemonic == "lea")
        .filter_map(|(at, (_, i))| {
            i.operands.first().filter(|o| o.ends_with("(%rip)"))?;
            Some(Lea {
                at,
                table: u64::from_str_radix(i.comment?.split_whitespace().next()?, 16).ok()?,
                dest: family(i.operands.get(1)?)?,
            })
        })
        .collect()
}

/// Loads through a table base, each followed in its block by the add and the
/// jump that complete the pattern, for every table the base register is
/// loaded with somewhere in the function.
fn candidates(parsed: &[(u64, Ins)], leas: &[Lea]) -> Vec<Candidate> {
    let mut found = Vec::new();
    for (load, (_, ins)) in parsed.iter().enumerate() {
        let base = ins.operands.first().and_then(|o| scaled_base(o));
        let word = ins.operands.get(1).and_then(|d| family(d));
        let (Some(base), Some(word)) = (base, word) else {
            continue;
        };
        if !matches!(ins.mnemonic, "movslq" | "movsxd") {
            continue;
        }
        let after = &parsed[load + 1..(load + 1 + WINDOW).min(parsed.len())];
        let Some(jmp) = completes(after, &base, &word) else {
            continue;
        };
        let jmp = load + 1 + jmp;
        for table in leas.iter().filter(|l| l.dest == base).map(|l| l.table) {
            if !found
                .iter()
                .any(|c: &Candidate| c.load == load && c.table == table)
            {
                found.push(Candidate {
                    table,
                    base: base.clone(),
                    load,
                    jmp,
                });
            }
        }
    }
    found
}

/// The index of the `jmp *` through the sum, if `after` adds the base to the
/// word and jumps through the result, with no write to the word, the base or
/// the sum before then.
fn completes(after: &[(u64, Ins)], base: &str, word: &str) -> Option<usize> {
    let mut sum: Option<String> = None;
    for (i, (_, ins)) in after.iter().enumerate() {
        match &sum {
            None => {
                let regs: Vec<Option<String>> = ins.operands.iter().map(|o| family(o)).collect();
                let adds = ins.mnemonic == "add"
                    && regs.len() == 2
                    && regs.iter().flatten().any(|f| f == base)
                    && regs.iter().flatten().any(|f| f == word);
                if adds {
                    sum = regs[1].clone();
                } else if ends_block(ins) || disturbs(ins, &[base, word]) {
                    return None;
                }
            }
            Some(total) => {
                let through = ins.mnemonic == "jmp"
                    && ins.operands.len() == 1
                    && ins.operands[0]
                        .strip_prefix('*')
                        .and_then(family)
                        .as_deref()
                        == Some(total);
                if through {
                    return Some(i);
                }
                if ends_block(ins) || disturbs(ins, &[total]) {
                    return None;
                }
            }
        }
    }
    None
}

/// The control-flow graph of one function.
struct Graph {
    succ: Vec<Vec<usize>>,
    leader: Vec<bool>,
}

impl Graph {
    /// `None` when the graph cannot be completed: an indirect jump that is no
    /// candidate's, or a branch to somewhere that is no instruction of the
    /// function and no tail call.
    fn build(
        parsed: &[(u64, Ins)],
        at: &HashMap<u64, usize>,
        span: (u64, u64),
        active: &[Candidate],
        targets: &HashMap<u64, Vec<u64>>,
    ) -> Option<Graph> {
        let n = parsed.len();
        let mut succ = vec![Vec::new(); n];
        let mut leader = vec![false; n];
        leader[0] = true;
        for (i, (_, ins)) in parsed.iter().enumerate() {
            let next = (i + 1 < n).then_some(i + 1);
            if ins.mnemonic == "(bad)" {
                return None;
            }
            if is_branch(ins) {
                if let Some(next) = next {
                    leader[next] = true;
                }
                let unconditional = ins.mnemonic == "jmp";
                match direct_target(ins) {
                    Some(t) if (span.0..span.1).contains(&t) => {
                        let to = *at.get(&t)?;
                        leader[to] = true;
                        succ[i].push(to);
                    }
                    Some(_) if unconditional => {}
                    Some(_) => return None,
                    None if unconditional => {
                        let table = active.iter().find(|c| c.jmp == i)?.table;
                        for t in targets.get(&table)? {
                            let to = *at.get(t)?;
                            leader[to] = true;
                            succ[i].push(to);
                        }
                    }
                    None => return None,
                }
                if !unconditional {
                    succ[i].extend(next);
                }
            } else if ends_block(ins) {
                if let Some(next) = next {
                    leader[next] = true;
                }
            } else {
                succ[i].extend(next);
            }
        }
        Some(Graph { succ, leader })
    }

    /// Whether the candidate's load, add and jump share a block, and the
    /// base register holds its table at the load along every path.
    fn reads_table(&self, parsed: &[(u64, Ins)], c: &Candidate, leas: &[Lea]) -> bool {
        if self.leader[c.load + 1..=c.jmp].iter().any(|&l| l) {
            return false;
        }
        let good: Vec<bool> = (0..parsed.len())
            .map(|i| {
                leas.iter()
                    .any(|l| l.at == i && l.table == c.table && l.dest == c.base)
            })
            .collect();
        self.always_defined(parsed, &c.base, &good, c.load)
    }

    /// Reaching definitions of register `reg` at instruction `at`: true when
    /// on every path from the entry the last definition is one of `good`.
    fn always_defined(&self, parsed: &[(u64, Ins)], reg: &str, good: &[bool], at: usize) -> bool {
        // 0: not reached yet, 1: defined by a good instruction, 2: anything else.
        let mut state = vec![0u8; parsed.len()];
        // Code the graph does not reach from the entry (a landing pad the
        // unwinder enters) may arrive with anything in the register. Nops
        // that only pad between blocks are not entered by anyone, nor is the
        // block they fall into reached through them.
        let mut padding = vec![false; parsed.len()];
        let reached = loop {
            let mut reached = vec![false; parsed.len()];
            for (i, edges) in self.succ.iter().enumerate() {
                if !padding[i] {
                    for &s in edges {
                        reached[s] = true;
                    }
                }
            }
            let mut grown = false;
            for i in 1..parsed.len() {
                if !reached[i] && !padding[i] && is_padding(&parsed[i].1) {
                    padding[i] = true;
                    grown = true;
                }
            }
            if !grown {
                break reached;
            }
        };
        let mut work = Vec::new();
        for (i, seen) in reached.iter().enumerate() {
            if i == 0 || (!seen && !padding[i]) {
                state[i] = 2;
                work.push(i);
            }
        }
        while let Some(i) = work.pop() {
            let ins = &parsed[i].1;
            let out = match writes(ins) {
                Writes::Reg(f) if f == reg => {
                    if good[i] {
                        1
                    } else {
                        2
                    }
                }
                Writes::Any if ins.mnemonic.starts_with("call") && !CALLER_SAVED.contains(&reg) => {
                    state[i]
                }
                Writes::Any => 2,
                _ => state[i],
            };
            for &s in &self.succ[i] {
                let merged = if state[s] == 0 {
                    out
                } else {
                    state[s].max(out)
                };
                if merged != state[s] {
                    state[s] = merged;
                    work.push(s);
                }
            }
        }
        state[at] == 1
    }
}

/// Proven table bases grouped by the section a caller maps them to.
pub fn group(
    proofs: &[Proof],
    section_of: impl Fn(u64) -> Option<usize>,
) -> HashMap<usize, Vec<u64>> {
    let mut grouped: HashMap<usize, Vec<u64>> = HashMap::new();
    for p in proofs {
        if let Some(k) = section_of(p.table) {
            grouped.entry(k).or_default().push(p.table);
        }
    }
    for bases in grouped.values_mut() {
        bases.sort_unstable();
        bases.dedup();
    }
    grouped
}

#[cfg(test)]
mod tests {
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
        for odd in ["rdtsc", "lahf", "cwtd", "xgetbv", "cmpxchg %rbx,(%rdi)"] {
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
        });
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

    #[test]
    fn registers_alias_across_widths() {
        assert_eq!(family("%eax"), family("%rax"));
        assert_eq!(family("%r10d"), family("%r10"));
        assert_ne!(family("%r10"), family("%r11"));
    }
}
