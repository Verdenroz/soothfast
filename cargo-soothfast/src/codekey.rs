//! A hash of the machine code an item executed, read from a bench binary.
//!
//! The functions come from a callgrind run (see `soothfast_measure::callgrind`),
//! so two binaries that hash a function set alike ran the same instructions
//! over the same constants. Addresses are normalized out: branch targets and
//! rip-relative displacements become symbolic, and a data reference becomes
//! the content of the symbol (or, with no sized symbol, the section) it points
//! into. Anything that cannot be resolved leaves the function unhashed, which
//! the caller sees as a miss. aarch64 `adrp`+`add` immediates carry layout and
//! only ever cause misses.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use soothfast_registry::fnv1a;

use crate::jumptable;
use crate::linkmap::Extent;

/// Stands for the bench binary in a stored function set, so two builds at
/// different paths name the same functions.
pub const EXE: &str = "@exe";

/// The `.llvm.N` segment is per-build; the rest of the name, an addend
/// included, is the identity.
fn strip_llvm(name: &str) -> Cow<'_, str> {
    let Some(at) = name.find(".llvm.") else {
        return Cow::Borrowed(name);
    };
    let rest = &name[at + ".llvm.".len()..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    Cow::Owned(format!("{}{}", &name[..at], &rest[digits..]))
}

/// Callgrind's name for the frames under `main`, which is no symbol.
const BELOW_MAIN: &str = "(below main)";

/// Prefix of a portable name for code callgrind could only name by address
/// (a symbol without a size, a PLT stub): the label objdump gives it.
const LABELED: &str = "at:";

/// A symbol without callgrind's `'N` marker for functions inside a recursion
/// cycle.
fn unmarked(sym: &str) -> &str {
    match sym.rsplit_once('\'') {
        Some((name, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => sym,
    }
}

/// `functions` as a stored entry keeps them: the object that is `bin` named
/// [`EXE`] with its symbols normalized, every other object reduced to its
/// path (its content decides what ran in it), sorted and deduplicated.
pub fn portable(bin: &Path, functions: &[(String, String)]) -> Vec<(String, String)> {
    let canonical = bin.canonicalize().ok();
    let is_bin = |ob: &str| Path::new(ob) == bin || canonical.as_deref() == Some(Path::new(ob));
    let set: BTreeSet<(String, String)> = functions
        .iter()
        .filter(|(ob, sym)| !(is_bin(ob) && sym == BELOW_MAIN))
        .map(|(ob, sym)| {
            if is_bin(ob) {
                (EXE.to_string(), strip_llvm(unmarked(sym)).to_string())
            } else {
                (ob.clone(), String::new())
            }
        })
        .collect();
    set.into_iter().collect()
}

struct Section {
    name: String,
    vma: u64,
    size: u64,
    offset: u64,
    code: bool,
    contents: bool,
}

struct Symbol {
    addr: u64,
    size: u64,
    name: String,
}

fn parse_sections(listing: &str) -> Vec<Section> {
    let mut lines = listing.lines();
    let mut sections = Vec::new();
    while let Some(header) = lines.next() {
        let f: Vec<&str> = header.split_whitespace().collect();
        if f.len() < 7 || f[0].parse::<u32>().is_err() {
            continue;
        }
        let Some(flags) = lines.next() else { break };
        if !flags.contains("ALLOC") || flags.contains("THREAD_LOCAL") {
            continue;
        }
        let hex = |s: &str| u64::from_str_radix(s, 16).ok();
        if let (Some(size), Some(vma), Some(offset)) = (hex(f[2]), hex(f[3]), hex(f[5])) {
            sections.push(Section {
                name: f[1].to_string(),
                vma,
                size,
                offset,
                code: flags.contains("CODE"),
                contents: flags.contains("CONTENTS"),
            });
        }
    }
    sections
}

/// Sized symbols from `nm -S`, ordered by address.
fn parse_symbols(listing: &str) -> Vec<Symbol> {
    let mut symbols: Vec<Symbol> = listing
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let [addr, size, _, name] = f[..] else {
                return None;
            };
            let size = u64::from_str_radix(size, 16).ok().filter(|&s| s > 0)?;
            Some(Symbol {
                addr: u64::from_str_radix(addr, 16).ok()?,
                size,
                name: name.to_string(),
            })
        })
        .collect();
    symbols.sort_by_key(|s| s.addr);
    symbols
}

/// Addresses that name no data to hash, keyed by address:
/// - Rust data symbols `nm` lists without a size are zero sized statics,
///   whose address code takes and whose content is nothing (`zst:NAME`);
/// - `__stop_SEC` is the linker's one-past-the-end of a section. It cannot be
///   dereferenced, and often sits at the start of whatever follows, which
///   code reads through its own references (`bound:NAME`). A `__start_SEC`
///   is not named here: it is a real element address, so it resolves as data.
///
/// A sized symbol at the same address is resolved first and wins.
fn parse_zero_sized(listing: &str, sections: &[Section]) -> HashMap<u64, String> {
    let mut named: Vec<(u64, String)> = listing
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let (addr, name) = match f[..] {
                [addr, _, name] => (addr, name),
                [addr, size, _, name] if u64::from_str_radix(size, 16) == Ok(0) => (addr, name),
                _ => return None,
            };
            let addr = u64::from_str_radix(addr, 16).ok()?;
            let in_data = sections
                .iter()
                .any(|s| !s.code && addr >= s.vma && addr - s.vma < s.size);
            if !in_data {
                return None;
            }
            if name.starts_with("__stop_") {
                return Some((addr, format!("bound:{name}")));
            }
            (name.starts_with("_R") || name.starts_with("_ZN"))
                .then(|| (addr, format!("zst:{}", strip_llvm(name))))
        })
        .collect();
    named.sort();
    named.into_iter().rev().collect()
}

/// What a dynamic relocation writes into its slot at load time.
#[derive(Clone)]
enum Target {
    /// An address inside this image.
    Relative(u64),
    /// A named symbol, with its addend.
    Symbol(String),
}

struct Reloc {
    offset: u64,
    target: Target,
}

/// Dynamic relocations from `objdump -R`, ordered by slot address.
fn parse_relocs(listing: &str) -> Vec<Reloc> {
    let mut relocs: Vec<Reloc> = listing
        .lines()
        .filter_map(|line| {
            let [offset, _, value] = line.split_whitespace().collect::<Vec<_>>()[..] else {
                return None;
            };
            let target = match value.strip_prefix("*ABS*+0x") {
                Some(addend) => Target::Relative(u64::from_str_radix(addend, 16).ok()?),
                None => Target::Symbol(value.to_string()),
            };
            Some(Reloc {
                offset: u64::from_str_radix(offset, 16).ok()?,
                target,
            })
        })
        .collect();
    relocs.sort_by_key(|r| r.offset);
    relocs
}

/// A piece of data a walk can enter.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Node {
    Symbol(usize),
    Section(usize),
    /// An input section the link map names, inside an output section.
    Input(usize),
    /// One entry of a fixed-size constant pool, by start and size.
    Entry(u64, u64),
}

/// What an address resolves to.
enum Place {
    /// Code, named; the layout never enters it.
    Code(String),
    /// An offset into data, plus the GOT slot name when objdump gave one.
    Data(Node, u64, String),
    /// One past the end of a section: the name, and the section before it,
    /// which the pointer can still reach backwards.
    Bounded(String, Node),
}

/// One canonical serialization of the data reachable from a root. Nodes are
/// numbered in visit order and an edge back to a visited node is written as
/// its number, so a cycle ends and the text does not depend on addresses.
#[derive(Default)]
struct Walk {
    order: HashMap<Node, usize>,
    out: Vec<u8>,
}

/// Turns an address into something that does not move with the layout.
struct Resolver<'a> {
    sections: &'a [Section],
    symbols: &'a [Symbol],
    relocs: &'a [Reloc],
    zero_sized: &'a HashMap<u64, String>,
    inputs: &'a [Extent],
    image: &'a [u8],
    roots: HashMap<Node, Option<u64>>,
}

impl Resolver<'_> {
    fn section_at(&self, addr: u64) -> Option<usize> {
        self.sections
            .iter()
            .position(|s| addr >= s.vma && addr - s.vma < s.size)
    }

    fn symbol_at(&self, addr: u64) -> Option<usize> {
        let i = self
            .symbols
            .partition_point(|s| s.addr <= addr)
            .checked_sub(1)?;
        let s = &self.symbols[i];
        (addr - s.addr < s.size).then_some(i)
    }

    /// `hint` is the symbol objdump printed next to the address, used only
    /// for code that has no sized symbol (PLT stubs) and GOT slots.
    fn place(&self, addr: u64, hint: Option<&str>) -> Option<Place> {
        let section = self.section_at(addr)?;
        let code = self.sections[section].code;
        if let Some(i) = self.symbol_at(addr)
            && self.section_at(self.symbols[i].addr) == Some(section)
        {
            let off = addr - self.symbols[i].addr;
            return Some(if code {
                Place::Code(format!("{}+{off:x}", strip_llvm(&self.symbols[i].name)))
            } else {
                Place::Data(Node::Symbol(i), off, String::new())
            });
        }
        if let Some(name) = self.zero_sized.get(&addr) {
            match name.strip_prefix("bound:__stop_") {
                None => return Some(Place::Code(name.clone())),
                // With no such section ending here, resolve as ordinary data.
                Some(sec) => {
                    let ends_here = |s: &Section| s.name == sec && s.vma + s.size == addr;
                    if let Some(i) = self.sections.iter().position(ends_here) {
                        return Some(Place::Bounded(name.clone(), Node::Section(i)));
                    }
                }
            }
        }
        if code {
            return hint.map(|h| Place::Code(strip_llvm(h).into_owned()));
        }
        let slot = hint.filter(|h| h.ends_with("$got")).unwrap_or_default();
        let (node, start) = match self.input_at(addr) {
            Some(i) if self.inputs[i].entry > 0 => {
                let e = &self.inputs[i];
                let start = e.vma + (addr - e.vma) / e.entry * e.entry;
                (Node::Entry(start, e.entry), start)
            }
            Some(i) => (Node::Input(i), self.inputs[i].vma),
            None => (Node::Section(section), self.sections[section].vma),
        };
        Some(Place::Data(node, addr - start, slot.to_string()))
    }

    fn input_at(&self, addr: u64) -> Option<usize> {
        let i = self
            .inputs
            .partition_point(|e| e.vma <= addr)
            .checked_sub(1)?;
        (addr - self.inputs[i].vma < self.inputs[i].size).then_some(i)
    }

    /// Section, address and size of a node.
    fn extent(&self, node: Node) -> Option<(usize, u64, u64)> {
        match node {
            Node::Symbol(i) => {
                let s = &self.symbols[i];
                Some((self.section_at(s.addr)?, s.addr, s.size))
            }
            Node::Section(i) => Some((i, self.sections[i].vma, self.sections[i].size)),
            Node::Input(i) => {
                let e = &self.inputs[i];
                Some((self.section_at(e.vma)?, e.vma, e.size))
            }
            Node::Entry(vma, size) => Some((self.section_at(vma)?, vma, size)),
        }
    }

    /// Append `node` to the walk. A relocated slot holds a layout-dependent
    /// value (or zero) in the file, so its bytes are blanked and what it
    /// points at is walked in their place.
    fn walk(&self, node: Node, walk: &mut Walk) -> Option<()> {
        if let Some(n) = walk.order.get(&node) {
            walk.out.extend(format!("ref#{n};").bytes());
            return Some(());
        }
        let n = walk.order.len();
        walk.order.insert(node, n);
        let (section, addr, size) = self.extent(node)?;
        let s = &self.sections[section];
        let contents = s.contents;
        let mut bytes = if contents {
            let start = usize::try_from(s.offset.checked_add(addr.checked_sub(s.vma)?)?).ok()?;
            let end = start.checked_add(usize::try_from(size).ok()?)?;
            self.image.get(start..end)?.to_vec()
        } else {
            format!("zero{size}").into_bytes()
        };
        let first = self.relocs.partition_point(|r| r.offset < addr);
        let last = self.relocs.partition_point(|r| r.offset < addr + size);
        if contents {
            for r in &self.relocs[first..last] {
                let slot = (r.offset - addr) as usize;
                bytes[slot..(slot + 8).min(size as usize)].fill(0);
            }
        }
        walk.out.extend(format!("node#{n}:{size}:").bytes());
        walk.out.extend(bytes);
        for i in first..last {
            let (slot, target) = (self.relocs[i].offset - addr, self.relocs[i].target.clone());
            walk.out.extend(format!("@{slot:x}=").bytes());
            match target {
                Target::Symbol(name) => walk
                    .out
                    .extend(format!("sym:{}", strip_llvm(&name)).bytes()),
                Target::Relative(to) => match self.place(to, None)? {
                    Place::Code(name) => walk.out.extend(name.bytes()),
                    Place::Bounded(name, next) => {
                        walk.out.extend(format!("{name}(").bytes());
                        self.walk(next, walk)?;
                        walk.out.push(b')');
                    }
                    Place::Data(next, off, _) => {
                        walk.out.extend(format!("+{off:x}(").bytes());
                        self.walk(next, walk)?;
                        walk.out.push(b')');
                    }
                },
            }
            walk.out.push(b';');
        }
        Some(())
    }

    /// Bytes of `size` at `addr` in the file image, for a section with contents.
    fn read(&self, addr: u64, size: u64) -> Option<&[u8]> {
        let s = &self.sections[self.section_at(addr)?];
        if !s.contents {
            return None;
        }
        let start = usize::try_from(s.offset.checked_add(addr - s.vma)?).ok()?;
        self.image
            .get(start..start.checked_add(usize::try_from(size).ok()?)?)
    }

    /// For each proven `lea`, the hash of the jump table it takes and the
    /// offset into its section, for every table that decodes. A table decodes
    /// when its whole input section is words that, from the nearest proven
    /// base at or before them, land inside the function (`start..end`).
    /// Anything else stays raw bytes.
    fn decode_tables(
        &self,
        proofs: &[jumptable::Proof],
        start: u64,
        end: u64,
    ) -> HashMap<u64, (u64, u64)> {
        let table_input = |table: u64| self.table_input(table);
        let mut tables = HashMap::new();
        for (j, bases) in jumptable::group(proofs, table_input) {
            let Some(hash) = self.decode(j, &bases, start, end) else {
                continue;
            };
            for p in proofs.iter().filter(|p| table_input(p.table) == Some(j)) {
                tables.insert(p.lea, (hash, p.table - self.inputs[j].vma));
            }
        }
        tables
    }

    /// The input section a table at `table` may be decoded from.
    fn table_input(&self, table: u64) -> Option<usize> {
        self.input_at(table)
            .filter(|&j| self.inputs[j].entry == 0 && self.symbol_at(table).is_none())
    }

    /// For each candidate table that decodes, the addresses it jumps to.
    fn table_targets(&self, tables: &[u64], start: u64, end: u64) -> HashMap<u64, Vec<u64>> {
        let candidates: Vec<jumptable::Proof> = tables
            .iter()
            .map(|&table| jumptable::Proof { lea: 0, table })
            .collect();
        let mut targets = HashMap::new();
        for (j, bases) in jumptable::group(&candidates, |t| self.table_input(t)) {
            let Some(words) = self.decode_words(j, &bases, start, end) else {
                continue;
            };
            let vma = self.inputs[j].vma;
            let index = |addr: u64| ((addr - vma) / 4) as usize;
            for (k, &base) in bases.iter().enumerate() {
                let upto = bases.get(k + 1).map_or(words.len(), |&next| index(next));
                targets.insert(base, words[index(base)..upto].to_vec());
            }
        }
        targets
    }

    fn decode(&self, input: usize, bases: &[u64], start: u64, end: u64) -> Option<u64> {
        let words = self.decode_words(input, bases, start, end)?;
        let mut text = format!("{}:", self.inputs[input].size);
        for target in words {
            text.push_str(&format!("{:x},", target - start));
        }
        Some(fnv1a(text.as_bytes()))
    }

    /// The place in `start..end` each word of the input section jumps to,
    /// measured from the nearest base at or before it. `None` unless every
    /// word lands there and the section is nothing but such words.
    fn decode_words(&self, input: usize, bases: &[u64], start: u64, end: u64) -> Option<Vec<u64>> {
        let e = &self.inputs[input];
        let relocated = self.relocs.partition_point(|r| r.offset < e.vma)
            != self.relocs.partition_point(|r| r.offset < e.vma + e.size);
        if !e.size.is_multiple_of(4) || bases.first() != Some(&e.vma) || relocated {
            return None;
        }
        let mut targets = Vec::new();
        for (i, word) in self
            .read(e.vma, e.size)?
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
        {
            let at = e.vma + 4 * i as u64;
            let base = bases[bases.partition_point(|&b| b <= at) - 1];
            let target = base.wrapping_add_signed(i64::from(i32::from_le_bytes(*word)));
            if target < start || target >= end {
                return None;
            }
            targets.push(target);
        }
        Some(targets)
    }

    /// Hash of everything reachable from `node`. Memoized per root only: a
    /// node reached inside another root's walk saw that walk's visit order.
    fn root_hash(&mut self, node: Node) -> Option<u64> {
        if let Some(h) = self.roots.get(&node) {
            return *h;
        }
        let mut walk = Walk::default();
        let h = self.walk(node, &mut walk).map(|_| fnv1a(&walk.out));
        self.roots.insert(node, h);
        h
    }

    fn resolve(&mut self, addr: u64, hint: Option<&str>) -> Option<String> {
        // Reading a GOT entry yields what it relocates to, so that alone names
        // it. Elsewhere a relocated address starts an object (a vtable) that
        // is read past its first slot.
        let in_got = self
            .section_at(addr)
            .is_some_and(|i| self.sections[i].name.starts_with(".got"));
        if in_got && let Ok(i) = self.relocs.binary_search_by_key(&addr, |r| r.offset) {
            let value = match self.relocs[i].target.clone() {
                Target::Symbol(name) => strip_llvm(&name).into_owned(),
                Target::Relative(to) => self.describe(to, None)?,
            };
            return Some(format!("slot:{value}"));
        }
        self.describe(addr, hint)
    }

    fn describe(&mut self, addr: u64, hint: Option<&str>) -> Option<String> {
        Some(match self.place(addr, hint)? {
            Place::Code(name) => name,
            Place::Bounded(name, node) => format!("{name}:{:016x}", self.root_hash(node)?),
            Place::Data(node, off, slot) => {
                let label = match node {
                    Node::Symbol(_) | Node::Input(_) | Node::Entry(..) => "data",
                    Node::Section(i) => &self.sections[i].name,
                };
                format!("{label}:{:016x}+{off:x}{slot}", self.root_hash(node)?)
            }
        })
    }

    /// One disassembled instruction (address column already removed).
    fn normalize(&mut self, body: &str) -> Option<String> {
        if let Some((head, tail)) = body.rsplit_once("# ") {
            let (addr, hint) = split_target(tail)?;
            let target = self.resolve(addr, hint)?;
            return Some(format!("{}# {target}", strip_rip_displacement(head)));
        }
        let (operands, hint) = match body.find('<') {
            Some(i) if body.ends_with('>') => {
                (body[..i].trim_end(), Some(&body[i + 1..body.len() - 1]))
            }
            _ => (body.trim_end(), None),
        };
        let tokens: Vec<&str> = operands.split_whitespace().collect();
        if let [.., mnemonic, target] = tokens[..]
            && is_branch(mnemonic)
            && let Ok(addr) = u64::from_str_radix(target, 16)
        {
            let prefix = &tokens[..tokens.len() - 1];
            return Some(format!(
                "{} {}",
                prefix.join(" "),
                self.resolve(addr, hint)?
            ));
        }
        Some(body.to_string())
    }
}

fn is_branch(mnemonic: &str) -> bool {
    mnemonic.starts_with('j') || mnemonic.starts_with("call") || mnemonic.starts_with("loop")
}

/// `3f000 <sym+0x10>` or a bare `3f000`.
fn split_target(tail: &str) -> Option<(u64, Option<&str>)> {
    let addr = u64::from_str_radix(tail.split_whitespace().next()?, 16).ok()?;
    let hint = tail
        .find('<')
        .and_then(|i| tail.rfind('>').map(|j| &tail[i + 1..j]));
    Some((addr, hint))
}

/// `lea    0x90d6(%rip),%rsi        ` becomes `lea    (%rip),%rsi        `.
fn strip_rip_displacement(operands: &str) -> String {
    let Some(at) = operands.find("(%rip)") else {
        return operands.to_string();
    };
    let start = operands[..at]
        .rfind(|c: char| !(c.is_ascii_hexdigit() || c == 'x' || c == '-'))
        .map_or(0, |i| i + 1);
    format!("{}{}", &operands[..start], &operands[at..])
}

/// One function being hashed: its extent, its instructions as disassembled,
/// and the normalized text once they are read.
struct Span {
    start: u64,
    end: u64,
    names: Vec<String>,
    instrs: Vec<(u64, String)>,
    text: String,
    resolved: bool,
}

impl Span {
    /// Normalize the instructions. A jump table the instructions prove (see
    /// `jumptable`) is hashed as the places in this function it jumps to.
    fn read(&mut self, resolver: &mut Resolver) {
        let proofs = jumptable::prove(&self.instrs, (self.start, self.end), &|tables| {
            resolver.table_targets(tables, self.start, self.end)
        });
        let tables = resolver.decode_tables(&proofs, self.start, self.end);
        for (addr, body) in std::mem::take(&mut self.instrs) {
            let table = tables
                .get(&addr)
                .and_then(|&(hash, off)| table_operand(&body, hash, off));
            match table.or_else(|| resolver.normalize(&body)) {
                Some(n) => {
                    self.text.push_str(&n);
                    self.text.push('\n');
                }
                None => self.resolved = false,
            }
        }
    }
}

/// `lea T(%rip),%rA` with the table in place of its address.
fn table_operand(body: &str, hash: u64, off: u64) -> Option<String> {
    let (head, _) = body.rsplit_once("# ")?;
    Some(format!(
        "{}# table:{hash:016x}+{off:x}",
        strip_rip_displacement(head)
    ))
}

/// Normalized hash per requested function, read off an `objdump -d` stream.
/// Copies of one name (per-CGU instances) hash together, so any copy changing
/// changes the name. A name with an unresolvable copy is left out.
///
/// Code named by address (`0x...`) or by an `at:` label has no extent in the
/// symbol table; it runs from its objdump label to the next one. `labels`
/// receives the `at:` name of each wanted address.
fn hash_functions(
    resolver: &mut Resolver,
    wanted: &BTreeSet<String>,
    mut disassembly: impl BufRead,
    labels: &mut HashMap<String, String>,
) -> HashMap<String, u64> {
    let mut spans: Vec<Span> = Vec::new();
    for s in resolver.symbols {
        let name = strip_llvm(&s.name);
        if !wanted.contains(name.as_ref()) {
            continue;
        }
        match spans.iter_mut().find(|sp| sp.start == s.addr) {
            Some(sp) => sp.names.push(name.to_string()),
            None => spans.push(Span {
                start: s.addr,
                end: s.addr + s.size,
                names: vec![name.to_string()],
                instrs: Vec::new(),
                text: String::new(),
                resolved: true,
            }),
        }
    }
    spans.sort_by_key(|s| s.start);

    let mut current: Option<usize> = None;
    let mut labeled: Option<(String, String, bool)> = None;
    let mut labeled_done: Vec<(String, Option<u64>)> = Vec::new();
    let mut finish = |labeled: &mut Option<(String, String, bool)>| {
        if let Some((key, text, resolved)) = labeled.take() {
            let hash = (resolved && !text.is_empty()).then(|| fnv1a(text.as_bytes()));
            labeled_done.push((key, hash));
        }
    };
    let mut line = Vec::new();
    while matches!(disassembly.read_until(b'\n', &mut line), Ok(n) if n > 0) {
        let text = String::from_utf8_lossy(&line);
        if let Some((addr, name)) = text
            .trim_end()
            .strip_suffix(">:")
            .and_then(|t| t.split_once(" <"))
            && let Ok(addr) = u64::from_str_radix(addr, 16)
        {
            finish(&mut labeled);
            let hex = format!("0x{addr:016x}");
            let key = format!("{LABELED}{name}");
            if !name.starts_with('.') && (wanted.contains(&hex) || wanted.contains(&key)) {
                if wanted.contains(&hex) {
                    labels.insert(hex, key.clone());
                }
                labeled = Some((key, String::new(), true));
            }
        } else if let Some((addr, body)) = text.split_once(':')
            && let Ok(addr) = u64::from_str_radix(addr.trim(), 16)
        {
            if current.is_none_or(|c| addr < spans[c].start || addr >= spans[c].end) {
                let after = spans.partition_point(|s| s.start <= addr);
                current = after.checked_sub(1).filter(|&c| addr < spans[c].end);
            }
            if let Some(c) = current {
                spans[c].instrs.push((addr, body.trim().to_string()));
            }
            if let Some((_, text, resolved)) = &mut labeled {
                match resolver.normalize(body.trim()) {
                    Some(n) => {
                        text.push_str(&n);
                        text.push('\n');
                    }
                    None => *resolved = false,
                }
            }
        }
        line.clear();
    }
    finish(&mut labeled);
    for span in &mut spans {
        span.read(resolver);
    }
    let mut copies: HashMap<String, Vec<Option<u64>>> = HashMap::new();
    for (key, hash) in labeled_done {
        copies.entry(key).or_default().push(hash);
    }
    for span in spans {
        let hash = (span.resolved && !span.text.is_empty()).then(|| fnv1a(span.text.as_bytes()));
        for name in span.names {
            copies.entry(name).or_default().push(hash);
        }
    }
    copies
        .into_iter()
        .filter_map(|(name, mut hashes)| {
            hashes.sort();
            let all: Option<Vec<u64>> = hashes.into_iter().collect();
            let bytes: Vec<u8> = all?.iter().flat_map(|h| h.to_le_bytes()).collect();
            Some((name, fnv1a(&bytes)))
        })
        .collect()
}

fn command_text(program: &str, args: &[&str], bin: &Path) -> Option<String> {
    let out = Command::new(program).args(args).arg(bin).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The requested functions of one bench binary, hashed.
pub struct Binary {
    hashes: HashMap<String, u64>,
    /// Code the wanted set named by address, to its `at:` name.
    labels: HashMap<String, String>,
}

impl Binary {
    /// Hash `wanted` (portable names) in `bin`. `None` where `nm` or `objdump`
    /// is missing or fails; a function that cannot be named uniquely or
    /// resolved is simply absent.
    pub fn open(bin: &Path, wanted: &BTreeSet<String>, inputs: &[Extent]) -> Option<Binary> {
        let sections = parse_sections(&command_text("objdump", &["-h"], bin)?);
        let listing = command_text("nm", &["-S", "--defined-only"], bin)?;
        let symbols = parse_symbols(&listing);
        let zero_sized = parse_zero_sized(&listing, &sections);
        // Fails on a binary with no dynamic section, which has no relocations.
        let relocs = parse_relocs(&command_text("objdump", &["-R"], bin).unwrap_or_default());
        let image = std::fs::read(bin).ok()?;
        let mut resolver = Resolver {
            sections: &sections,
            symbols: &symbols,
            relocs: &relocs,
            zero_sized: &zero_sized,
            inputs,
            image: &image,
            roots: HashMap::new(),
        };
        let mut child = Command::new("objdump")
            .args(["-d", "--no-show-raw-insn"])
            .arg(bin)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let mut labels = HashMap::new();
        let hashes = hash_functions(&mut resolver, wanted, BufReader::new(stdout), &mut labels);
        child
            .wait()
            .ok()?
            .success()
            .then_some(Binary { hashes, labels })
    }

    #[cfg(test)]
    pub fn with_hashes(hashes: HashMap<String, u64>) -> Binary {
        Binary {
            hashes,
            labels: HashMap::new(),
        }
    }

    /// `functions` with each address replaced by its `at:` name; `None` if
    /// one has no label here.
    pub fn name_addresses(
        &self,
        functions: Vec<(String, String)>,
    ) -> Option<Vec<(String, String)>> {
        functions
            .into_iter()
            .map(|(ob, sym)| {
                if ob == EXE && sym.starts_with("0x") {
                    Some((ob, self.labels.get(&sym)?.clone()))
                } else {
                    Some((ob, sym))
                }
            })
            .collect()
    }

    /// The key of `functions` (as [`portable`] returns them) in this binary,
    /// or `None` if any is unhashed here or any foreign object is unreadable.
    pub fn code_key(&self, functions: &[(String, String)]) -> Option<String> {
        let mut text = String::new();
        for (ob, sym) in functions {
            if ob != EXE {
                writeln!(text, "f {ob} {:016x}", file_digest(ob)?).ok()?;
            } else {
                writeln!(text, "x {sym} {:016x}", self.hashes.get(sym)?).ok()?;
            }
        }
        Some(format!("{:016x}", fnv1a(text.as_bytes())))
    }
}

/// Content digest of a foreign object, read once per path. Callgrind's `???`
/// is valgrind's own trampolines, which the host key pins by valgrind version.
fn file_digest(path: &str) -> Option<u64> {
    if path == "???" {
        return Some(0);
    }
    static MEMO: OnceLock<Mutex<HashMap<String, Option<u64>>>> = OnceLock::new();
    let mut memo = MEMO.get_or_init(Default::default).lock().ok()?;
    *memo
        .entry(path.to_string())
        .or_insert_with(|| std::fs::read(path).ok().map(|b| fnv1a(&b)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const SECTIONS: &str = "\
Idx Name          Size      VMA               LMA               File off  Algn
  0 .text         00000100  0000000000001000  0000000000001000  00001000  2**4
                  CONTENTS, ALLOC, LOAD, READONLY, CODE
  1 .rodata       00000020  0000000000002000  0000000000002000  00002000  2**4
                  CONTENTS, ALLOC, LOAD, READONLY, DATA
  2 .tbss         00000008  0000000000003000  0000000000003000  00003000  2**3
                  ALLOC, THREAD_LOCAL
";

    const SYMBOLS: &str = "\
0000000000001000 0000000000000020 T _ZN3foo3bar17h1E
0000000000001040 0000000000000020 t _ZN3foo3baz17h2E
0000000000002000 0000000000000008 r anon.abc.0.llvm.77
0000000000002010 r unsized
";

    fn image(constant: u8, rest: u8) -> Vec<u8> {
        let mut image = vec![0; 0x3000];
        image[0x2000..0x2008].fill(constant);
        image[0x2008..0x2020].fill(rest);
        image
    }

    fn hashes(sections: &str, symbols: &str, image: &[u8], dis: &str) -> HashMap<String, u64> {
        hashes_relocated(sections, symbols, "", image, dis)
    }

    fn hashes_relocated(
        sections: &str,
        symbols: &str,
        relocs: &str,
        image: &[u8],
        dis: &str,
    ) -> HashMap<String, u64> {
        let (sections, symbols) = (parse_sections(sections), parse_symbols(symbols));
        let relocs = parse_relocs(relocs);
        let mut resolver = Resolver {
            sections: &sections,
            symbols: &symbols,
            relocs: &relocs,
            zero_sized: &HashMap::new(),
            inputs: &[],
            image,
            roots: HashMap::new(),
        };
        let wanted = ["_ZN3foo3bar17h1E", "_ZN3foo3baz17h2E"]
            .map(String::from)
            .into();
        hash_functions(
            &mut resolver,
            &wanted,
            Cursor::new(dis.as_bytes()),
            &mut HashMap::new(),
        )
    }

    const DIS: &str = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tlea    0x1000(%rip),%rax        # 2000 <anon.abc.0.llvm.77>
    1007:\tcall   1040 <_ZN3foo3baz17h2E>
    100c:\tjne    1018 <_ZN3foo3bar17h1E+0x18>
    1012:\tmov    $0x2a,%eax
    1017:\tret
";

    fn bar(h: &HashMap<String, u64>) -> Option<u64> {
        h.get("_ZN3foo3bar17h1E").copied()
    }

    #[test]
    fn a_moved_function_over_the_same_constants_hashes_alike() {
        let moved = "\
0000000000009000 <_ZN3foo3bar17h1E>:
    9000:\tlea    0x1234(%rip),%rax        # a100 <anon.abc.0.llvm.12>
    9007:\tcall   9040 <_ZN3foo3baz17h2E>
    900c:\tjne    9018 <_ZN3foo3bar17h1E+0x18>
    9012:\tmov    $0x2a,%eax
    9017:\tret
";
        let moved_sections = SECTIONS
            .replace("0000000000001000", "0000000000009000")
            .replace("0000000000002000", "000000000000a100");
        let moved_symbols = SYMBOLS
            .replace("0000000000001000", "0000000000009000")
            .replace("0000000000001040", "0000000000009040")
            .replace("0000000000002000", "000000000000a100")
            .replace("0000000000002010", "000000000000a110")
            .replace("llvm.77", "llvm.12");
        let base = hashes(SECTIONS, SYMBOLS, &image(7, 1), DIS);
        let layout = hashes(&moved_sections, &moved_symbols, &image(7, 1), moved);
        assert!(bar(&base).is_some());
        assert_eq!(bar(&base), bar(&layout));
    }

    #[test]
    fn a_changed_constant_or_opcode_changes_the_hash() {
        let base = bar(&hashes(SECTIONS, SYMBOLS, &image(7, 1), DIS));
        let constant = bar(&hashes(SECTIONS, SYMBOLS, &image(8, 1), DIS));
        let opcode = bar(&hashes(
            SECTIONS,
            SYMBOLS,
            &image(7, 1),
            &DIS.replace("ret", "hlt"),
        ));
        assert_ne!(base, constant);
        assert_ne!(base, opcode);
    }

    #[test]
    fn a_reference_into_unsized_data_hashes_its_section() {
        let dis = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tlea    0x1000(%rip),%rax        # 2010 <unsized>
    1007:\tret
";
        let a = bar(&hashes(SECTIONS, SYMBOLS, &image(7, 1), dis));
        let b = bar(&hashes(SECTIONS, SYMBOLS, &image(7, 2), dis));
        assert!(a.is_some());
        assert_ne!(a, b);
    }

    #[test]
    fn a_rust_data_symbol_without_a_size_is_zero_sized() {
        let sections = parse_sections(SECTIONS);
        let listing = "\
0000000000001040 t _ZN3foo4code17h1E
0000000000002008 r _RNvCs1_3foo5EMPTY.llvm.9
0000000000002010 r _IO_stdin_used
0000000000002018 0000000000000008 r _RNvCs1_3foo5SIZED
";
        assert_eq!(
            parse_zero_sized(listing, &sections),
            HashMap::from([(0x2008, "zst:_RNvCs1_3foo5EMPTY".to_string())])
        );
    }

    #[test]
    fn a_zero_sized_static_does_not_hide_the_object_at_its_address() {
        let (sections, symbols) = (parse_sections(SECTIONS), parse_symbols(SYMBOLS));
        let zero_sized = HashMap::from([(0x2000, "zst:_RNvCs1_3foo5EMPTY".to_string())]);
        let wanted: BTreeSet<String> = ["_ZN3foo3bar17h1E".to_string()].into();
        let hash = |image: &[u8]| {
            let mut resolver = Resolver {
                sections: &sections,
                symbols: &symbols,
                relocs: &[],
                zero_sized: &zero_sized,
                inputs: &[],
                image,
                roots: HashMap::new(),
            };
            let hashes = hash_functions(
                &mut resolver,
                &wanted,
                Cursor::new(DIS.as_bytes()),
                &mut HashMap::new(),
            );
            bar(&hashes)
        };
        assert!(hash(&image(7, 1)).is_some());
        assert_ne!(hash(&image(7, 1)), hash(&image(8, 1)));
    }

    #[test]
    fn a_table_hashes_every_slot_not_just_the_first() {
        let symbols = SYMBOLS.replace("0000000000000008 r anon", "0000000000000010 r anon");
        let relocs = |second: u64| {
            format!(
                "0000000000002000 R_X86_64_RELATIVE  *ABS*+0x0000000000001000\n\
                 0000000000002008 R_X86_64_RELATIVE  *ABS*+0x{second:016x}"
            )
        };
        let at = |second: u64| {
            bar(&hashes_relocated(
                SECTIONS,
                &symbols,
                &relocs(second),
                &image(7, 1),
                DIS,
            ))
        };
        assert!(at(0x1000).is_some());
        assert_ne!(at(0x1000), at(0x1040));
    }

    #[test]
    fn a_relocated_slot_hashes_by_its_target_not_its_bytes() {
        let at = |addend: u64, slot: u8| {
            let mut image = image(7, 1);
            image[0x2000..0x2008].fill(slot);
            let relocs = format!("0000000000002000 R_X86_64_RELATIVE  *ABS*+0x{addend:016x}");
            bar(&hashes_relocated(SECTIONS, SYMBOLS, &relocs, &image, DIS))
        };
        assert!(at(0x1000, 0).is_some());
        assert_ne!(at(0x1000, 0), at(0x1040, 0));
        assert_eq!(at(0x1000, 0), at(0x1000, 9));
    }

    #[test]
    fn a_symbolic_relocation_hashes_by_its_name() {
        let with = |name: &str| {
            let relocs = format!("0000000000002000 R_X86_64_GLOB_DAT  {name}");
            bar(&hashes_relocated(
                SECTIONS,
                SYMBOLS,
                &relocs,
                &image(7, 1),
                DIS,
            ))
        };
        assert_ne!(with("alpha"), with("beta"));
        assert_eq!(with("alpha"), with("alpha"));
    }

    #[test]
    fn llvm_suffixes_drop_but_addends_stay() {
        assert_eq!(strip_llvm("foo.llvm.12+0x10"), "foo+0x10");
        assert_eq!(strip_llvm("foo.llvm.12"), "foo");
        assert_eq!(strip_llvm("foo+0x10"), "foo+0x10");
    }

    const CYCLE_SYMBOLS: &str = "\
0000000000001000 0000000000000020 T _ZN3foo3bar17h1E
0000000000001040 0000000000000020 T _ZN3foo3baz17h2E
0000000000002000 0000000000000010 r table_a
0000000000002010 0000000000000008 r table_b
";
    const CYCLE_RELOCS: &str = "\
0000000000002000 R_X86_64_RELATIVE  *ABS*+0x0000000000002010
0000000000002010 R_X86_64_RELATIVE  *ABS*+0x0000000000002000
";
    const CYCLE_DIS: &str = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tlea    0x1000(%rip),%rax        # 2000 <table_a>
    1007:\tret
0000000000001040 <_ZN3foo3baz17h2E>:
    1040:\tlea    0x1000(%rip),%rax        # 2010 <table_b>
    1047:\tret
";

    fn cycle(image: &[u8], dis: &str) -> (Option<u64>, Option<u64>) {
        let h = hashes_relocated(SECTIONS, CYCLE_SYMBOLS, CYCLE_RELOCS, image, dis);
        (bar(&h), h.get("_ZN3foo3baz17h2E").copied())
    }

    #[test]
    fn a_pointer_cycle_hashes_by_content_whichever_side_is_entered_first() {
        let base = image(7, 1);
        let mut moved = base.clone();
        moved[0x2008] = 9;
        let swapped = "\
0000000000001000 <_ZN3foo3baz17h2E>:
    1000:\tlea    0x1000(%rip),%rax        # 2010 <table_b>
    1007:\tret
0000000000001040 <_ZN3foo3bar17h1E>:
    1040:\tlea    0x1000(%rip),%rax        # 2000 <table_a>
    1047:\tret
";
        let swapped_symbols = CYCLE_SYMBOLS
            .replace(
                "1000 0000000000000020 T _ZN3foo3bar",
                "9999 0000000000000020 T _ZN3foo3bar",
            )
            .replace(
                "1040 0000000000000020 T _ZN3foo3baz",
                "1000 0000000000000020 T _ZN3foo3baz",
            )
            .replace(
                "9999 0000000000000020 T _ZN3foo3bar",
                "1040 0000000000000020 T _ZN3foo3bar",
            );
        let other = hashes_relocated(SECTIONS, &swapped_symbols, CYCLE_RELOCS, &base, swapped);
        let (bar_a, baz_a) = cycle(&base, CYCLE_DIS);
        assert!(bar_a.is_some() && baz_a.is_some());
        assert_eq!(
            (bar_a, baz_a),
            (bar(&other), other.get("_ZN3foo3baz17h2E").copied())
        );
        let (bar_b, baz_b) = cycle(&moved, CYCLE_DIS);
        assert_ne!(bar_a, bar_b);
        assert_ne!(baz_a, baz_b);
    }

    #[test]
    fn a_link_map_extent_bounds_a_reference_into_unnamed_data() {
        let dis = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tlea    0x1000(%rip),%rax        # 2008 <anon>
    1007:\tret
";
        let (sections, symbols) = (parse_sections(SECTIONS), parse_symbols(SYMBOLS));
        let hash = |inputs: &[Extent], byte: usize| {
            let mut image = image(7, 1);
            image[byte] = 9;
            let mut resolver = Resolver {
                sections: &sections,
                symbols: &symbols,
                relocs: &[],
                zero_sized: &HashMap::new(),
                inputs,
                image: &image,
                roots: HashMap::new(),
            };
            let wanted = ["_ZN3foo3bar17h1E".to_string()].into();
            let hashes = hash_functions(
                &mut resolver,
                &wanted,
                Cursor::new(dis.as_bytes()),
                &mut HashMap::new(),
            );
            bar(&hashes)
        };
        let input = [Extent {
            vma: 0x2008,
            size: 8,
            entry: 0,
        }];
        assert_eq!(hash(&input, 0x2018), hash(&input, 0x201c));
        assert_ne!(hash(&input, 0x2008), hash(&input, 0x2009));
        assert_ne!(hash(&[], 0x2018), hash(&[], 0x201c));
    }

    #[test]
    fn a_constant_pool_hashes_only_the_entry_a_function_reads() {
        let dis = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tlea    0x1000(%rip),%rax        # 2008 <anon>
    1007:\tret
";
        let (sections, symbols) = (parse_sections(SECTIONS), parse_symbols(SYMBOLS));
        let pool = [Extent {
            vma: 0x2008,
            size: 16,
            entry: 8,
        }];
        let hash = |byte: usize| {
            let mut image = image(7, 1);
            image[byte] = 9;
            let mut resolver = Resolver {
                sections: &sections,
                symbols: &symbols,
                relocs: &[],
                zero_sized: &HashMap::new(),
                inputs: &pool,
                image: &image,
                roots: HashMap::new(),
            };
            let wanted = ["_ZN3foo3bar17h1E".to_string()].into();
            let hashes = hash_functions(
                &mut resolver,
                &wanted,
                Cursor::new(dis.as_bytes()),
                &mut HashMap::new(),
            );
            bar(&hashes)
        };
        assert_eq!(hash(0x2010), hash(0x2017));
        assert_ne!(hash(0x2008), hash(0x2009));
    }

    const FN_SYMBOLS: &str = "0000000000001000 0000000000000020 T _ZN3foo3bar17h1E\n";

    /// A function switching through the table at `table`, whose words point
    /// at `targets` (addresses in the function), with `between` after the lea.
    fn switch(table: u64, targets: &[u64], between: &str) -> (String, Vec<u8>) {
        let dis = format!(
            "0000000000001000 <_ZN3foo3bar17h1E>:\n\
             \x20   1000:\tlea    0x1000(%rip),%rcx        # {table:x} <t>\n{between}\
             \x20   1007:\tmovslq (%rcx,%rax,4),%rax\n\
             \x20   100b:\tadd    %rcx,%rax\n\
             \x20   100e:\tjmp    *%rax\n\
             \x20   1010:\tret\n\
             \x20   1014:\tret\n\
             \x20   1018:\tret\n\
             \x20   101c:\tret\n"
        );
        let mut image = vec![0; 0x3000];
        for (i, target) in targets.iter().enumerate() {
            let word = (*target as i64 - table as i64) as i32;
            let at = table as usize + 4 * i;
            image[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
        (dis, image)
    }

    fn table_hash(extent: (u64, u64), dis: &str, image: &[u8]) -> Option<u64> {
        let (sections, symbols) = (parse_sections(SECTIONS), parse_symbols(FN_SYMBOLS));
        let inputs = [Extent {
            vma: extent.0,
            size: extent.1,
            entry: 0,
        }];
        let mut resolver = Resolver {
            sections: &sections,
            symbols: &symbols,
            relocs: &[],
            zero_sized: &HashMap::new(),
            inputs: &inputs,
            image,
            roots: HashMap::new(),
        };
        let wanted = ["_ZN3foo3bar17h1E".to_string()].into();
        let hashes = hash_functions(
            &mut resolver,
            &wanted,
            Cursor::new(dis.as_bytes()),
            &mut HashMap::new(),
        );
        bar(&hashes)
    }

    const TARGETS: [u64; 4] = [0x1010, 0x1014, 0x1018, 0x101c];

    #[test]
    fn a_proven_table_hashes_the_same_wherever_it_and_the_code_sit() {
        let hash = |table: u64, targets: &[u64]| {
            let (dis, image) = switch(table, targets, "");
            table_hash((table, 16), &dis, &image)
        };
        let base = hash(0x2008, &TARGETS);
        assert!(base.is_some());
        assert_eq!(base, hash(0x2010, &TARGETS));
        let mut other = TARGETS;
        other[2] = 0x1014;
        assert_ne!(base, hash(0x2008, &other));
    }

    #[test]
    fn a_table_with_a_write_to_its_base_register_stays_raw() {
        let hash = |table: u64| {
            let (dis, image) = switch(table, &TARGETS, "    1004:\tmov    %rbx,%rcx\n");
            table_hash((table, 16), &dis, &image)
        };
        assert_ne!(hash(0x2008), hash(0x2010));
    }

    #[test]
    fn a_table_with_a_word_outside_the_function_stays_raw() {
        let hash = |table: u64| {
            let mut targets = TARGETS;
            targets[3] = 0x1500;
            let (dis, image) = switch(table, &targets, "");
            table_hash((table, 16), &dis, &image)
        };
        assert_ne!(hash(0x2008), hash(0x2010));
    }

    #[test]
    fn two_tables_in_one_section_decode_from_their_own_bases() {
        let dis = |t1: u64, t2: u64| {
            format!(
                "0000000000001000 <_ZN3foo3bar17h1E>:\n\
                 \x20   1000:\tlea    0x1000(%rip),%rcx        # {t1:x} <t>\n\
                 \x20   1002:\tmovslq (%rcx,%rax,4),%rax\n\
                 \x20   1004:\tadd    %rcx,%rax\n\
                 \x20   1006:\tjmp    *%rax\n\
                 \x20   1008:\tlea    0x1000(%rip),%rdx        # {t2:x} <t>\n\
                 \x20   100a:\tmovslq (%rdx,%rax,4),%rax\n\
                 \x20   100c:\tadd    %rdx,%rax\n\
                 \x20   100e:\tjmp    *%rax\n\
                 \x20   1010:\tret\n\
                 \x20   1012:\tret\n\
                 \x20   1014:\tret\n\
                 \x20   1016:\tret\n"
            )
        };
        let hash = |t1: u64| {
            let t2 = t1 + 8;
            let mut image = vec![0; 0x3000];
            for (i, target) in [0x1010u64, 0x1008, 0x1014, 0x1016].iter().enumerate() {
                let base = if i < 2 { t1 } else { t2 };
                let word = (*target as i64 - base as i64) as i32;
                let at = t1 as usize + 4 * i;
                image[at..at + 4].copy_from_slice(&word.to_le_bytes());
            }
            table_hash((t1, 16), &dis(t1, t2), &image)
        };
        assert!(hash(0x2000).is_some());
        assert_eq!(hash(0x2000), hash(0x2010));
    }

    #[test]
    fn a_stop_symbol_hashes_the_section_it_ends_and_not_the_one_after() {
        let sections = parse_sections(
            "\
Idx Name          Size      VMA               LMA               File off  Algn
  0 .text         00000100  0000000000001000  0000000000001000  00001000  2**4
                  CONTENTS, ALLOC, LOAD, READONLY, CODE
  1 .got          00000008  0000000000002000  0000000000002000  00002000  2**3
                  CONTENTS, ALLOC, LOAD, DATA
  2 linkme_x      00000008  0000000000002008  0000000000002008  00002008  2**3
                  CONTENTS, ALLOC, LOAD, DATA
  3 .after        00000020  0000000000002010  0000000000002010  00002010  2**3
                  CONTENTS, ALLOC, LOAD, DATA
",
        );
        let symbols = parse_symbols(FN_SYMBOLS);
        let relocs = parse_relocs("0000000000002000 R_X86_64_RELATIVE  *ABS*+0x0000000000002010");
        let dis = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tcmp    0x1000(%rip),%rax        # 2000 <_GLOBAL_OFFSET_TABLE_>
    1007:\tret
";
        let hash = |stops: &HashMap<u64, String>, at: usize, byte: u8| {
            let mut image = vec![0; 0x3000];
            image[at] = byte;
            let mut resolver = Resolver {
                sections: &sections,
                symbols: &symbols,
                relocs: &relocs,
                zero_sized: stops,
                inputs: &[],
                image: &image,
                roots: HashMap::new(),
            };
            let wanted = ["_ZN3foo3bar17h1E".to_string()].into();
            let hashes = hash_functions(
                &mut resolver,
                &wanted,
                Cursor::new(dis.as_bytes()),
                &mut HashMap::new(),
            );
            bar(&hashes)
        };
        let stops = HashMap::from([(0x2010, "bound:__stop_linkme_x".to_string())]);
        assert!(hash(&stops, 0x2018, 1).is_some());
        assert_eq!(hash(&stops, 0x2018, 1), hash(&stops, 0x2018, 2));
        assert_ne!(hash(&stops, 0x2009, 1), hash(&stops, 0x2009, 2));
        let none = HashMap::new();
        assert_ne!(hash(&none, 0x2018, 1), hash(&none, 0x2018, 2));
    }

    #[test]
    fn a_stop_symbol_is_named_and_a_start_symbol_is_not() {
        let sections = parse_sections(SECTIONS);
        let listing = "\
0000000000002008 r __stop_linkme_X
0000000000002010 r __start_linkme_Y
0000000000002018 r __stop_linkme_Z
0000000000002018 r __stop_linkme_A
";
        assert_eq!(
            parse_zero_sized(listing, &sections),
            HashMap::from([
                (0x2008, "bound:__stop_linkme_X".to_string()),
                (0x2018, "bound:__stop_linkme_A".to_string()),
            ])
        );
    }

    #[test]
    fn an_unresolvable_reference_leaves_the_function_unhashed() {
        let dis = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tlea    0x1000(%rip),%rax        # 9000 <elsewhere>
";
        assert_eq!(bar(&hashes(SECTIONS, SYMBOLS, &image(7, 1), dis)), None);
    }

    #[test]
    fn displacements_are_dropped_and_only_the_target_is_kept() {
        assert_eq!(
            strip_rip_displacement("lea    0x90d6(%rip),%rsi        "),
            "lea    (%rip),%rsi        "
        );
        assert_eq!(
            strip_rip_displacement("mov    -0x10(%rip),%rax"),
            "mov    (%rip),%rax"
        );
    }

    #[test]
    fn portable_names_the_bench_binary_and_reduces_other_objects_to_paths() {
        let f = |ob: &str, sym: &str| (ob.to_string(), sym.to_string());
        let functions = vec![
            f("/lib/libc.so.6", "memcpy"),
            f("/lib/libc.so.6", "memmove"),
            f("/w/target/bench", "b.llvm.9"),
            f("/w/target/bench", "a'2"),
            f("/w/target/bench", "(below main)"),
            f("/w/target/bench", "0x0000000000023eb0"),
        ];
        assert_eq!(
            portable(Path::new("/w/target/bench"), &functions),
            [
                f("/lib/libc.so.6", ""),
                f(EXE, "0x0000000000023eb0"),
                f(EXE, "a"),
                f(EXE, "b"),
            ]
        );
    }

    fn labeled(dis: &str) -> (HashMap<String, u64>, HashMap<String, String>) {
        let (sections, symbols) = (parse_sections(SECTIONS), parse_symbols(SYMBOLS));
        let mut resolver = Resolver {
            sections: &sections,
            symbols: &symbols,
            relocs: &[],
            zero_sized: &HashMap::new(),
            inputs: &[],
            image: &[],
            roots: HashMap::new(),
        };
        let wanted: BTreeSet<String> = ["0x0000000000024000", "0x0000000000024100"]
            .map(String::from)
            .into();
        let mut labels = HashMap::new();
        let hashes = hash_functions(
            &mut resolver,
            &wanted,
            Cursor::new(dis.as_bytes()),
            &mut labels,
        );
        (hashes, labels)
    }

    #[test]
    fn code_named_by_address_hashes_by_content_under_its_label() {
        let dis = |insn: &str| {
            format!(
                "0000000000024000 <_init>:\n   24000:\t{insn}\n0000000000024100 <.text>:\n   24100:\tret\n"
            )
        };
        let (ret, labels) = labeled(&dis("ret"));
        let (hlt, _) = labeled(&dis("hlt"));
        assert_eq!(
            labels.into_iter().collect::<Vec<_>>(),
            [("0x0000000000024000".to_string(), "at:_init".to_string())]
        );
        assert_ne!(ret["at:_init"], hlt["at:_init"]);
        assert!(!ret.contains_key("at:.text"));
    }

    #[test]
    fn an_address_takes_its_label_name_and_a_missing_one_has_none() {
        let mut binary = Binary::with_hashes(HashMap::from([("at:_init".to_string(), 1)]));
        binary
            .labels
            .insert("0x0000000000024000".into(), "at:_init".into());
        let named = binary
            .name_addresses(vec![(EXE.into(), "0x0000000000024000".into())])
            .expect("named");
        assert_eq!(named, [(EXE.to_string(), "at:_init".to_string())]);
        assert!(binary.code_key(&named).is_some());
        assert!(
            binary
                .name_addresses(vec![(EXE.into(), "0x0000000000000001".into())])
                .is_none()
        );
    }

    #[test]
    fn a_function_missing_from_the_binary_has_no_key() {
        let binary = Binary::with_hashes(HashMap::new());
        assert_eq!(binary.code_key(&[(EXE.into(), "gone".into())]), None);
    }

    #[test]
    fn hashing_a_function_of_the_running_test_binary() {
        let exe = std::env::current_exe().unwrap();
        let nm = command_text("nm", &["-S", "--defined-only"], &exe).unwrap();
        let name = nm
            .lines()
            .filter(|l| {
                l.split_whitespace()
                    .nth(2)
                    .is_some_and(|t| t == "t" || t == "T")
            })
            .filter_map(|l| l.split_whitespace().nth(3))
            .map(strip_llvm)
            .find(|n| n.starts_with("_R"))
            .unwrap()
            .to_string();
        let wanted = BTreeSet::from([name.clone()]);
        let functions = [(EXE.to_string(), name)];
        let first = Binary::open(&exe, &wanted, &[])
            .unwrap()
            .code_key(&functions);
        let second = Binary::open(&exe, &wanted, &[])
            .unwrap()
            .code_key(&functions);
        assert!(first.is_some());
        assert_eq!(first, second);
    }
}
