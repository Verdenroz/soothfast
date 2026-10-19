//! Jump tables proven by the instructions that use them, x86-64 AT&T syntax.
//!
//! A table of 32-bit code offsets changes whenever code moves, so its bytes
//! say nothing stable. Where one function reads a table through exactly this
//! dataflow, with nothing writing the registers in between and nothing
//! branching into the middle, the words are offsets from the table's base to
//! places in that function:
//!
//! ```text
//! lea    T(%rip),%rA
//! movslq (%rA,%rI,4),%rB
//! add    %rA,%rB          (either operand order)
//! jmp    *%rB
//! ```
//!
//! Any other shape is no proof, and the caller hashes the bytes.

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
    if m.starts_with('j')
        || starts(&[
            "cmp", "test", "push", "nop", "endbr", "ret", "ud2", "int3", "hlt", "prefetch",
        ])
    {
        return Writes::Nothing;
    }
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

/// Addresses some branch in the function jumps to.
fn branch_targets(instrs: &[(u64, Ins)]) -> HashSet<u64> {
    instrs
        .iter()
        .filter(|(_, i)| {
            i.mnemonic.starts_with('j')
                || i.mnemonic.starts_with("call")
                || i.mnemonic.starts_with("loop")
        })
        .filter_map(|(_, i)| {
            u64::from_str_radix(i.operands.first()?.split_whitespace().next()?, 16).ok()
        })
        .collect()
}

/// The `(%rA,%rI,4)` base register of a memory operand.
fn scaled_base(operand: &str) -> Option<String> {
    let inner = operand.strip_suffix(",4)")?.rsplit_once('(')?.1;
    let (base, _index) = inner.split_once(',')?;
    family(base)
}

/// Every table `instrs` (one function, in order) reads in the proven way.
pub fn prove(instrs: &[(u64, String)]) -> Vec<Proof> {
    let parsed: Vec<(u64, Ins)> = instrs
        .iter()
        .filter_map(|(addr, body)| Some((*addr, parse(body)?)))
        .collect();
    let targets = branch_targets(&parsed);
    let mut proofs = Vec::new();
    for (i, (addr, ins)) in parsed.iter().enumerate() {
        if ins.mnemonic != "lea" {
            continue;
        }
        let (Some(table), Some(base)) = (
            ins.comment
                .and_then(|c| u64::from_str_radix(c.split_whitespace().next()?, 16).ok()),
            ins.operands.get(1).and_then(|d| family(d)),
        ) else {
            continue;
        };
        if ins.operands.first().is_none_or(|o| !o.ends_with("(%rip)")) {
            continue;
        }
        if reads_table(
            &parsed[i + 1..(i + 1 + WINDOW).min(parsed.len())],
            &base,
            &targets,
        ) {
            proofs.push(Proof { lea: *addr, table });
        }
    }
    proofs
}

/// Whether `after` (the instructions following a `lea` into `base`) loads a
/// sign-extended word through `base`, adds `base`, and jumps through the sum,
/// with no write to the registers and no branch target in between.
fn reads_table(after: &[(u64, Ins)], base: &str, targets: &HashSet<u64>) -> bool {
    let mut loaded: Option<String> = None;
    let mut sum: Option<String> = None;
    for (addr, ins) in after {
        if targets.contains(addr) {
            return false;
        }
        match (&loaded, &sum) {
            (None, _) => {
                let is_load = matches!(ins.mnemonic, "movslq" | "movsxd")
                    && ins.operands.first().and_then(|o| scaled_base(o)).as_deref() == Some(base);
                if is_load {
                    loaded = ins.operands.get(1).and_then(|d| family(d));
                    if loaded.is_none() {
                        return false;
                    }
                } else if disturbs(ins, &[base]) {
                    return false;
                }
            }
            (Some(word), None) => {
                let regs: Vec<Option<String>> = ins.operands.iter().map(|o| family(o)).collect();
                let adds = ins.mnemonic == "add"
                    && regs.len() == 2
                    && regs.iter().flatten().any(|f| f == base)
                    && regs.iter().flatten().any(|f| f == word);
                if adds {
                    sum = regs[1].clone();
                } else if disturbs(ins, &[base, word]) {
                    return false;
                }
            }
            (Some(_), Some(total)) => {
                if ins.mnemonic == "jmp"
                    && ins.operands.len() == 1
                    && ins.operands[0]
                        .strip_prefix('*')
                        .and_then(family)
                        .as_deref()
                        == Some(total)
                {
                    return true;
                }
                if disturbs(ins, &[total]) {
                    return false;
                }
            }
        }
    }
    false
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

    fn code(lines: &[&str]) -> Vec<(u64, String)> {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| (0x1000 + 4 * i as u64, l.to_string()))
            .collect()
    }

    const TABLE: &[&str] = &[
        "lea    0x100d(%rip),%rcx        # 2010 <anon>",
        "movslq (%rcx,%rax,4),%rax",
        "add    %rcx,%rax",
        "jmp    *%rax",
    ];

    #[test]
    fn the_exact_dataflow_proves_a_table() {
        assert_eq!(
            prove(&code(TABLE)),
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
        ];
        assert_eq!(prove(&code(&swapped)).len(), 1);
    }

    #[test]
    fn a_write_to_the_base_register_breaks_the_proof() {
        let broken = [TABLE[0], "mov    %rbx,%rcx", TABLE[1], TABLE[2], TABLE[3]];
        assert!(prove(&code(&broken)).is_empty());
    }

    #[test]
    fn a_write_to_the_loaded_or_summed_register_breaks_the_proof() {
        let between = [TABLE[0], TABLE[1], "xor    %eax,%eax", TABLE[2], TABLE[3]];
        assert!(prove(&code(&between)).is_empty());
        let after = [TABLE[0], TABLE[1], TABLE[2], "inc    %rax", TABLE[3]];
        assert!(prove(&code(&after)).is_empty());
    }

    #[test]
    fn a_branch_into_the_middle_breaks_the_proof() {
        let mut instrs = code(&[
            "jne    1008 <f+0x8>",
            TABLE[0],
            TABLE[1],
            TABLE[2],
            TABLE[3],
        ]);
        assert!(prove(&instrs).is_empty());
        instrs[0].1 = "jne    1100 <f+0x100>".to_string();
        assert_eq!(prove(&instrs).len(), 1);
    }

    #[test]
    fn a_load_that_is_not_scaled_by_four_is_no_proof() {
        let wrong = [TABLE[0], "movslq (%rcx,%rax,8),%rax", TABLE[2], TABLE[3]];
        assert!(prove(&code(&wrong)).is_empty());
    }

    #[test]
    fn registers_alias_across_widths() {
        assert_eq!(family("%eax"), family("%rax"));
        assert_eq!(family("%r10d"), family("%r10"));
        assert_ne!(family("%r10"), family("%r11"));
    }
}
