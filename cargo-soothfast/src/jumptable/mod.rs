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
//!
//! The same load and add with no jump after them read a table of offsets to
//! data, such as the names an enum's `Debug` impl picks from. Its words move
//! whenever the data does, and are proven the same way.

use std::collections::{HashMap, HashSet};

/// A proven table base: the `lea` that takes it, and the address it takes.
#[derive(Debug, PartialEq)]
pub struct Proof {
    pub lea: u64,
    pub table: u64,
}

/// What [`prove`] found: the proven bases of jump tables and of data tables,
/// and for every table whose words decoded, the places they name.
#[derive(Default)]
pub struct Proven {
    pub proofs: Vec<Proof>,
    pub pointers: Vec<Proof>,
    pub targets: HashMap<u64, Vec<u64>>,
}

/// How far past a `lea` the rest of the pattern may sit.
const WINDOW: usize = 32;

/// How far past an add a data table's sum is followed for a register jump.
const REACH: usize = 512;

struct Ins<'a> {
    mnemonic: &'a str,
    operands: Vec<&'a str>,
    comment: Option<&'a str>,
}

const PREFIXES: &[&str] = &[
    "notrack", "bnd", "rex", "data16", "cs", "ds", "lock", "xacquire", "xrelease",
];

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
    Reg(Vec<String>),
    /// Anything may change, as at a call or an implicit-operand instruction.
    Any,
}

fn writes(ins: &Ins) -> Writes {
    let m = ins.mnemonic;
    let starts = |names: &[&str]| names.iter().any(|n| m.starts_with(n));
    let is = |names: &[&str]| names.contains(&m);
    let regs = |names: &[&str]| Writes::Reg(names.iter().map(|n| n.to_string()).collect());
    if starts(&["call", "rep"]) {
        return Writes::Any;
    }
    if let Some(implicit) = implicit_writes(m, ins.operands.len()) {
        return regs(implicit);
    }
    if starts(&["cmpxchg", "xchg", "xadd"]) {
        let mut regs: Vec<String> = ins.operands.iter().filter_map(|o| family(o)).collect();
        if m.starts_with("cmpxchg") {
            regs.retain(|r| ins.operands.last().and_then(|o| family(o)).as_ref() == Some(r));
            regs.push("a".into());
        }
        return Writes::Reg(regs);
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
        Some(f) => Writes::Reg(vec![f]),
        None => Writes::Nothing,
    }
}

/// The registers an instruction writes without naming them as operands.
fn implicit_writes(m: &str, operands: usize) -> Option<&'static [&'static str]> {
    Some(match m {
        "mul" | "mulb" | "mulw" | "mull" | "mulq" | "div" | "divb" | "divw" | "divl" | "divq"
        | "idiv" | "idivb" | "idivw" | "idivl" | "idivq" => &["a", "d"],
        _ if m.starts_with("imul") && operands == 1 => &["a", "d"],
        _ if m.starts_with("cmpxchg8b") || m.starts_with("cmpxchg16b") => &["a", "d"],
        "cltd" | "cqto" | "cwtd" => &["d"],
        "cltq" | "cwtl" | "cbtw" => &["a"],
        "leave" => &["sp", "bp"],
        _ if m.starts_with("loop") => &["c"],
        "cpuid" => &["a", "b", "c", "d"],
        "syscall" => &["a", "c", "r11"],
        "movsb" | "movsw" | "movsl" | "movsq" | "cmpsb" | "cmpsw" | "cmpsl" | "cmpsq" => {
            &["si", "di"]
        }
        "stosb" | "stosw" | "stosl" | "stosq" | "scasb" | "scasw" | "scasl" | "scasq" => &["di"],
        "lodsb" | "lodsw" | "lodsl" | "lodsq" => &["a", "si"],
        _ => return None,
    })
}

fn disturbs(ins: &Ins, regs: &[&str]) -> bool {
    match writes(ins) {
        Writes::Nothing => false,
        Writes::Any => true,
        Writes::Reg(fs) => fs.iter().any(|f| regs.contains(&f.as_str())),
    }
}

/// The `(%rA,%rI,4)` base register of a memory operand.
fn scaled_base(operand: &str) -> Option<String> {
    let inner = operand.strip_suffix(",4)")?.rsplit_once('(')?.1;
    let (base, _index) = inner.split_once(',')?;
    family(base)
}

/// The address an instruction's `# addr <sym>` comment, or a bare operand,
/// starts with.
pub fn address_of(text: &str) -> Option<u64> {
    u64::from_str_radix(text.split_whitespace().next()?, 16).ok()
}

/// `jmp *d(%rip)`: a tail call through a slot, and the address of that slot.
fn through_slot(ins: &Ins) -> Option<u64> {
    let slot = ins.operands.len() == 1
        && ins.operands[0].starts_with('*')
        && ins.operands[0].ends_with("(%rip)");
    slot.then(|| address_of(ins.comment?)).flatten()
}

/// `jmp *d(%rA)` or `jmp *(%rA)`: a jump through a pointer in memory, with no
/// index register, which a table jump always has.
fn loads_pointer(ins: &Ins) -> bool {
    ins.operands.len() == 1
        && ins.operands[0]
            .strip_prefix('*')
            .is_some_and(|o| unindexed_memory(o) && !o.ends_with("(%rip)"))
}

/// `jmp *%rA`, whose register `exits_through_loaded_pointers` must show holds
/// a pointer from memory.
fn through_loaded_register(ins: &Ins) -> bool {
    ins.operands.len() == 1
        && ins.operands[0]
            .strip_prefix('*')
            .is_some_and(|o| family(o).is_some())
}

fn unindexed_memory(operand: &str) -> bool {
    !operand.starts_with('%')
        && !operand.starts_with('$')
        && operand.ends_with(')')
        && !operand.contains(',')
}

fn direct_target(ins: &Ins) -> Option<u64> {
    is_jump(ins.mnemonic)
        .then(|| address_of(ins.operands.first()?))
        .flatten()
}

/// A jump or a loop, conditional or not.
pub fn is_jump(mnemonic: &str) -> bool {
    mnemonic.starts_with('j') || mnemonic.starts_with("loop")
}

/// A nop, as compilers pad between blocks.
fn is_padding(ins: &Ins) -> bool {
    ins.mnemonic.starts_with("nop") || (ins.mnemonic == "xchg" && ins.operands == ["%ax", "%ax"])
}

fn ends_block(ins: &Ins) -> bool {
    is_jump(ins.mnemonic)
        || matches!(ins.mnemonic, "ret" | "retq" | "ud2" | "hlt" | "int3")
        || is_unwind_resume(ins)
}

/// A call to `_Unwind_Resume`, which never returns, so nothing follows it.
fn is_unwind_resume(ins: &Ins) -> bool {
    ins.mnemonic.starts_with("call")
        && ins
            .operands
            .iter()
            .copied()
            .chain(ins.comment)
            .filter_map(|text| text.split_once("<_Unwind_Resume"))
            .any(|(_, tail)| tail.starts_with(['>', '@']))
}

const CALLER_SAVED: &[&str] = &["a", "c", "d", "si", "di", "r8", "r9", "r10", "r11"];

/// Where each candidate table jumps to, for those whose words decode.
pub type Decoder<'a> = dyn Fn(&[u64]) -> HashMap<u64, Vec<u64>> + 'a;

/// What the image says about the places a function's jumps leave for.
pub trait Exits {
    /// Whether control that jumps to `target` has left for good: it is the
    /// entry of a function outside this one, and no fragment of another.
    fn branch_leaves(&self, target: u64) -> bool;
    /// The same for a tail call through the slot at the address.
    fn slot_leaves(&self, slot: u64) -> bool;
    /// Whether this function is Rust, which has no computed goto.
    fn rust(&self) -> bool;
}

/// A table read as `movslq (%rA,%rI,4),%rB; add; jmp *%rD`, from one `lea`.
/// A `pointer` table ends at the add, whose sum is the address of data.
struct Candidate {
    table: u64,
    base: String,
    load: usize,
    /// The `jmp *`, or for a `pointer` table the add.
    end: usize,
    pointer: bool,
}

/// Every table `instrs` (one function, in order, spanning `span`) reads in
/// the proven way.
///
/// `decode` takes candidate table addresses and returns the places in the
/// function each one jumps to, for those whose words decode. `pointers` does
/// the same for the tables that end at the add, giving the address each word
/// names. The proof is a control-flow graph of the function, in which a table
/// jump has an edge to each place its table names:
///
/// - the load, add and `jmp *` (for a data table, the load and add) sit in
///   one basic block, with no write between to the registers that carry the
///   word or the sum;
/// - every definition of the base register that reaches the load is a `lea`
///   of the same table, along every path from the function's entry. A call
///   keeps the callee-saved registers and clobbers the rest; an instruction
///   that may write anything clobbers all.
///
/// The graph must be complete, and a path may end only at one of these:
///
/// - `ret` and the other instructions that end a block;
/// - a direct branch, conditional or not, out of the span to `Exits::branch_leaves`;
/// - `jmp *d(%rip)` through a slot for which `Exits::slot_leaves` holds;
/// - in a Rust function only, `jmp *d(%rA)` or `jmp *(%rA)` through unindexed
///   memory, and `jmp *%rA` where every path last loaded `%rA` that way. C
///   has labels as values, so such a jump can re-enter the span; Rust has
///   none, and an async state machine resumes through a switch.
///
/// Every other indirect jump is a proven table jump, or nothing is proven. A
/// branch to a fragment of another function (`.cold`) or into the middle of a
/// symbol is not an exit: it can come back. All exits assume control never
/// re-enters the span except through a call. A candidate that fails is
/// dropped, and the rest are checked again against the graph without it.
pub fn prove(
    instrs: &[(u64, String)],
    span: (u64, u64),
    decode: &Decoder,
    pointers: &Decoder,
    exits: &dyn Exits,
) -> Proven {
    let Some(parsed) = instrs
        .iter()
        .map(|(addr, body)| Some((*addr, parse(body)?)))
        .collect::<Option<Vec<(u64, Ins)>>>()
    else {
        return Proven::default();
    };
    let at: HashMap<u64, usize> = parsed
        .iter()
        .enumerate()
        .map(|(i, (a, _))| (*a, i))
        .collect();
    let leas = rip_leas(&parsed);
    let mut active = candidates(&parsed, &leas);
    loop {
        let jumps = decode(&tables_of(&active, false));
        let jumped: HashSet<u64> = jumps.keys().copied().collect();
        let mut data = tables_of(&active, true);
        data.retain(|t| !jumped.contains(t));
        let mut targets = pointers(&data);
        targets.extend(jumps);
        active.retain(|c| targets.contains_key(&c.table) && c.pointer != jumped.contains(&c.table));
        if active.is_empty() {
            return Proven::default();
        }
        let Some(graph) = Graph::build(&parsed, &at, span, &active, &targets, exits) else {
            return Proven::default();
        };
        if !graph.exits_through_loaded_pointers(&parsed) {
            return Proven::default();
        }
        let before = active.len();
        active.retain(|c| {
            graph.reads_table(&parsed, c, &leas)
                && (!c.pointer || graph.read_once(&parsed, c, &leas))
        });
        if active.len() == before {
            let proven: HashSet<u64> = active.iter().map(|c| c.table).collect();
            let data: HashSet<u64> = tables_of(&active, true).into_iter().collect();
            let (data_proofs, proofs) = leas
                .iter()
                .filter(|l| proven.contains(&l.table))
                .map(|l| Proof {
                    lea: parsed[l.at].0,
                    table: l.table,
                })
                .partition(|p| data.contains(&p.table));
            return Proven {
                proofs,
                pointers: data_proofs,
                targets,
            };
        }
    }
}

/// The distinct tables of the jump candidates, or of the data ones.
fn tables_of(active: &[Candidate], pointer: bool) -> Vec<u64> {
    let mut tables: Vec<u64> = active
        .iter()
        .filter(|c| c.pointer == pointer)
        .map(|c| c.table)
        .collect();
    tables.sort_unstable();
    tables.dedup();
    tables
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
                table: address_of(i.comment?)?,
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
        if !matches!(ins.mnemonic, "movslq" | "movsxd") || word == base {
            continue;
        }
        let Some((end, pointer)) = read_end(&parsed[load + 1..], &base, &word) else {
            continue;
        };
        let end = load + 1 + end;
        for table in leas.iter().filter(|l| l.dest == base).map(|l| l.table) {
            if !found
                .iter()
                .any(|c: &Candidate| c.load == load && c.table == table)
            {
                found.push(Candidate {
                    table,
                    base: base.clone(),
                    load,
                    end,
                    pointer,
                });
            }
        }
    }
    found
}

/// Where `after` ends reading a table through `base` and `word`: the index of
/// the `jmp *` through the sum of the two and `false`, or the index of the add
/// and `true` when the sum, in the word's own register so that the raw offset
/// dies in the add, then ends its life: the block ends, or an
/// instruction writes the sum register or a call clobbers it, with no register
/// jump before. `None` when nothing adds them within [`WINDOW`], one of them is
/// written first, a branch to an address or a register jump follows that is not
/// through the sum or is further than [`WINDOW`], or [`REACH`] runs out.
fn read_end(after: &[(u64, Ins)], base: &str, word: &str) -> Option<(usize, bool)> {
    let mut sum: Option<(usize, String)> = None;
    for (i, (_, ins)) in after.iter().enumerate() {
        let Some((add, total)) = &sum else {
            let regs: Vec<Option<String>> = ins.operands.iter().map(|o| family(o)).collect();
            let pair = |a: &str, b: &str| {
                regs.len() == 2 && regs[0].as_deref() == Some(a) && regs[1].as_deref() == Some(b)
            };
            let adds = ins.mnemonic == "add" && (pair(base, word) || pair(word, base));
            match regs.get(1).cloned().flatten() {
                Some(total) if adds && i < WINDOW => sum = Some((i, total)),
                _ if i >= WINDOW || ends_block(ins) || disturbs(ins, &[base, word]) => {
                    return None;
                }
                _ => {}
            }
            continue;
        };
        let through = ins.mnemonic == "jmp"
            && ins.operands.len() == 1
            && ins.operands[0]
                .strip_prefix('*')
                .and_then(family)
                .as_deref()
                == Some(total.as_str());
        if through && i < WINDOW {
            return Some((i, false));
        }
        let register_jump = ins.mnemonic == "jmp" && through_loaded_register(ins);
        if register_jump || direct_target(ins).is_some() {
            return None;
        }
        let call = ins.mnemonic.starts_with("call");
        let clobbered = if call {
            CALLER_SAVED.contains(&total.as_str())
        } else {
            disturbs(ins, &[total.as_str()])
        };
        if ends_block(ins) || clobbered {
            return (total.as_str() == word).then_some((*add, true));
        }
        if i >= REACH {
            return None;
        }
    }
    None
}

/// Whether `ins` reads register family `reg`, rather than only overwriting it.
fn reads_register(ins: &Ins, reg: &str) -> bool {
    let mentions = |operand: &str| {
        operand.split('%').skip(1).any(|rest| {
            let name: String = rest
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect();
            family(&format!("%{name}")).as_deref() == Some(reg)
        })
    };
    let Some((dest, sources)) = ins.operands.split_last() else {
        return false;
    };
    if sources.iter().any(|o| mentions(o)) {
        return true;
    }
    let overwrite = dest.starts_with('%')
        && ["mov", "lea", "pop", "set"]
            .iter()
            .any(|m| ins.mnemonic.starts_with(m));
    mentions(dest) && !overwrite
}

/// The control-flow graph of one function.
struct Graph {
    succ: Vec<Vec<usize>>,
    leader: Vec<bool>,
    /// Instructions the graph does not reach from the entry and that are not
    /// padding: landing pads the unwinder enters.
    orphans: Vec<usize>,
    has_call: bool,
    /// `jmp *%rA` instructions that are no table jump, with their register:
    /// exits, provided `exits_through_loaded_pointers` shows `%rA` holds a
    /// pointer from memory.
    pointer_exits: Vec<(usize, String)>,
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
        exits: &dyn Exits,
    ) -> Option<Graph> {
        let n = parsed.len();
        let mut succ = vec![Vec::new(); n];
        let mut leader = vec![false; n];
        leader[0] = true;
        let mut pointer_exits = Vec::new();
        for (i, (_, ins)) in parsed.iter().enumerate() {
            let next = (i + 1 < n).then_some(i + 1);
            if ins.mnemonic == "(bad)" {
                return None;
            }
            if is_jump(ins.mnemonic) {
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
                    Some(t) if exits.branch_leaves(t) => {}
                    Some(_) => return None,
                    None if unconditional
                        && through_slot(ins).is_some_and(|a| exits.slot_leaves(a)) => {}
                    None if unconditional => match active.iter().find(|c| !c.pointer && c.end == i)
                    {
                        Some(c) => {
                            for t in targets.get(&c.table)? {
                                let to = *at.get(t)?;
                                leader[to] = true;
                                succ[i].push(to);
                            }
                        }
                        None if exits.rust() && loads_pointer(ins) => {}
                        None if exits.rust() && through_loaded_register(ins) => {
                            let reg = ins.operands[0].strip_prefix('*').and_then(family)?;
                            pointer_exits.push((i, reg));
                        }
                        None => return None,
                    },
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
        let orphans = orphans(parsed, &succ);
        let has_call = parsed.iter().any(|(_, i)| i.mnemonic.starts_with("call"));
        Some(Graph {
            succ,
            leader,
            orphans,
            has_call,
            pointer_exits,
        })
    }

    /// Whether every register-indirect exit jumps through a register whose
    /// last definition, on every path, is a load of a pointer from memory.
    /// Each register is followed through the graph once for all its exits.
    fn exits_through_loaded_pointers(&self, parsed: &[(u64, Ins)]) -> bool {
        let mut by_reg: HashMap<&str, Vec<usize>> = HashMap::new();
        for (at, reg) in &self.pointer_exits {
            by_reg.entry(reg).or_default().push(*at);
        }
        by_reg.into_iter().all(|(reg, exits)| {
            let good: Vec<bool> = parsed
                .iter()
                .map(|(_, ins)| {
                    ins.mnemonic.starts_with("mov")
                        && ins.operands.len() == 2
                        && unindexed_memory(ins.operands[0])
                        && family(ins.operands[1]).as_deref() == Some(reg)
                })
                .collect();
            let state = self.reaching(parsed, reg, &good);
            exits.iter().all(|&at| state[at] == 1)
        })
    }

    /// Whether the candidate's load, add and jump share a block, and the
    /// base register holds its table at the load along every path.
    fn reads_table(&self, parsed: &[(u64, Ins)], c: &Candidate, leas: &[Lea]) -> bool {
        if self.leader[c.load + 1..=c.end].iter().any(|&l| l) {
            return false;
        }
        let mut good = vec![false; parsed.len()];
        for l in leas
            .iter()
            .filter(|l| l.table == c.table && l.dest == c.base)
        {
            good[l.at] = true;
        }
        self.always_defined(parsed, &c.base, &good, c.load)
    }

    /// Whether nothing but the candidate's load and add reads a data table's
    /// base register while it may still hold the table: any other read could
    /// see a raw offset the hash of the objects it names leaves out.
    fn read_once(&self, parsed: &[(u64, Ins)], c: &Candidate, leas: &[Lea]) -> bool {
        let mut gens = vec![false; parsed.len()];
        for l in leas
            .iter()
            .filter(|l| l.table == c.table && l.dest == c.base)
        {
            gens[l.at] = true;
        }
        let holds = self.may_hold(parsed, &c.base, &gens);
        parsed.iter().enumerate().all(|(i, (_, ins))| {
            i == c.load || i == c.end || !holds[i] || !reads_register(ins, &c.base)
        })
    }

    /// For each instruction, whether `reg` may hold a value one of the `gens`
    /// instructions defined on entering it. A landing pad may hold it whenever
    /// any does.
    fn may_hold(&self, parsed: &[(u64, Ins)], reg: &str, gens: &[bool]) -> Vec<bool> {
        let mut holds = vec![false; parsed.len()];
        let mut work = Vec::new();
        let reach = |from: &[usize], holds: &mut Vec<bool>, work: &mut Vec<usize>| {
            for &s in from {
                if !holds[s] {
                    holds[s] = true;
                    work.push(s);
                }
            }
        };
        for (i, _) in gens.iter().enumerate().filter(|(_, g)| **g) {
            reach(&self.succ[i], &mut holds, &mut work);
        }
        if gens.iter().any(|g| *g) {
            reach(&self.orphans, &mut holds, &mut work);
        }
        while let Some(i) = work.pop() {
            let ins = &parsed[i].1;
            let kept = match writes(ins) {
                Writes::Reg(fs) => !fs.iter().any(|f| f == reg),
                Writes::Any => ins.mnemonic.starts_with("call") && !CALLER_SAVED.contains(&reg),
                Writes::Nothing => true,
            };
            if kept || gens[i] {
                reach(&self.succ[i], &mut holds, &mut work);
            }
        }
        holds
    }

    /// Reaching definitions of register `reg` at instruction `at`: true when
    /// on every path from the entry the last definition is one of `good`.
    fn always_defined(&self, parsed: &[(u64, Ins)], reg: &str, good: &[bool], at: usize) -> bool {
        self.reaching(parsed, reg, good)[at] == 1
    }

    /// For each instruction, how `reg` is defined on entering it: 0 not
    /// reached, 1 by a good instruction on every path, 2 otherwise.
    fn reaching(&self, parsed: &[(u64, Ins)], reg: &str, good: &[bool]) -> Vec<u8> {
        let mut state = vec![0u8; parsed.len()];
        // Only a call unwinds, and it restores callee-saved registers to their
        // values at the call: an orphan gets their join, else unknown.
        let from_calls = !CALLER_SAVED.contains(&reg) && self.has_call;
        let mut work = vec![0];
        state[0] = 2;
        if !from_calls {
            for &i in &self.orphans {
                state[i] = 2;
                work.push(i);
            }
        }
        while let Some(i) = work.pop() {
            let ins = &parsed[i].1;
            let out = match writes(ins) {
                Writes::Reg(fs) if fs.iter().any(|f| f == reg) => {
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
            // `_Unwind_Resume` carries the exception out of this frame, never
            // into one of its pads.
            let pads: &[usize] =
                if from_calls && ins.mnemonic.starts_with("call") && !is_unwind_resume(ins) {
                    &self.orphans
                } else {
                    &[]
                };
            for &s in self.succ[i].iter().chain(pads) {
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
        state
    }
}

/// Code the graph does not reach from the entry (a landing pad the unwinder
/// enters), which may arrive with anything in a register. Nops that only pad
/// between blocks are not entered by anyone, nor is the block they fall into
/// reached through them.
fn orphans(parsed: &[(u64, Ins)], succ: &[Vec<usize>]) -> Vec<usize> {
    let mut padding = vec![false; parsed.len()];
    let reached = loop {
        let mut reached = vec![false; parsed.len()];
        for (i, edges) in succ.iter().enumerate() {
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
    (1..parsed.len())
        .filter(|&i| !reached[i] && !padding[i])
        .collect()
}

/// Table bases grouped by the section a caller maps them to.
pub fn group(
    tables: impl IntoIterator<Item = u64>,
    section_of: impl Fn(u64) -> Option<usize>,
) -> HashMap<usize, Vec<u64>> {
    let mut grouped: HashMap<usize, Vec<u64>> = HashMap::new();
    for table in tables {
        if let Some(k) = section_of(table) {
            grouped.entry(k).or_default().push(table);
        }
    }
    for bases in grouped.values_mut() {
        bases.sort_unstable();
        bases.dedup();
    }
    grouped
}

#[cfg(test)]
mod tests;
