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

/// Prefix of a PLT stub's portable name. A stub is a trampoline fixed by the
/// symbol it names; the function it reaches is listed under its own object.
const STUB: &str = "plt:";

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
}

/// What an address resolves to.
enum Place {
    /// Code, named; the layout never enters it.
    Code(String),
    /// An offset into data, plus the GOT slot name when objdump gave one.
    Data(Node, u64, String),
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
        if code {
            return hint.map(|h| Place::Code(strip_llvm(h).into_owned()));
        }
        let slot = hint.filter(|h| h.ends_with("$got")).unwrap_or_default();
        Some(Place::Data(
            Node::Section(section),
            addr - self.sections[section].vma,
            slot.to_string(),
        ))
    }

    /// Section, address and size of a node.
    fn extent(&self, node: Node) -> Option<(usize, u64, u64)> {
        match node {
            Node::Symbol(i) => {
                let s = &self.symbols[i];
                Some((self.section_at(s.addr)?, s.addr, s.size))
            }
            Node::Section(i) => Some((i, self.sections[i].vma, self.sections[i].size)),
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
        Some(match self.place(addr, hint)? {
            Place::Code(name) => name,
            Place::Data(node, off, slot) => {
                let label = match node {
                    Node::Symbol(_) => "data",
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

/// One function being hashed: its extent, and the normalized text so far.
struct Span {
    start: u64,
    end: u64,
    names: Vec<String>,
    text: String,
    resolved: bool,
}

/// Normalized hash per requested function, read off an `objdump -d` stream.
/// Copies of one name (per-CGU instances) hash together, so any copy changing
/// changes the name. A name with an unresolvable copy is left out.
fn hash_functions(
    resolver: &mut Resolver,
    wanted: &BTreeSet<String>,
    mut disassembly: impl BufRead,
    stubs: &mut HashMap<String, String>,
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
                text: String::new(),
                resolved: true,
            }),
        }
    }
    spans.sort_by_key(|s| s.start);

    let mut current: Option<usize> = None;
    let mut line = Vec::new();
    while matches!(disassembly.read_until(b'\n', &mut line), Ok(n) if n > 0) {
        let text = String::from_utf8_lossy(&line);
        if let Some((addr, name)) = text
            .trim_end()
            .strip_suffix(">:")
            .and_then(|t| t.split_once(" <"))
            && let Ok(addr) = u64::from_str_radix(addr, 16)
        {
            let hex = format!("0x{addr:016x}");
            if wanted.contains(&hex) && name.ends_with("@plt") {
                stubs.insert(hex, name.to_string());
            }
        } else if let Some((addr, body)) = text.split_once(':')
            && let Ok(addr) = u64::from_str_radix(addr.trim(), 16)
        {
            if current.is_none_or(|c| addr < spans[c].start || addr >= spans[c].end) {
                let after = spans.partition_point(|s| s.start <= addr);
                current = after.checked_sub(1).filter(|&c| addr < spans[c].end);
            }
            if let Some(c) = current {
                match resolver.normalize(body.trim()) {
                    Some(n) => {
                        spans[c].text.push_str(&n);
                        spans[c].text.push('\n');
                    }
                    None => spans[c].resolved = false,
                }
            }
        }
        line.clear();
    }
    let mut copies: HashMap<String, Vec<Option<u64>>> = HashMap::new();
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
    /// Symbolless code the wanted set named by address, to the name objdump
    /// gave the stub at that address.
    stubs: HashMap<String, String>,
}

impl Binary {
    /// Hash `wanted` (portable names) in `bin`. `None` where `nm` or `objdump`
    /// is missing or fails; a function that cannot be named uniquely or
    /// resolved is simply absent.
    pub fn open(bin: &Path, wanted: &BTreeSet<String>) -> Option<Binary> {
        let sections = parse_sections(&command_text("objdump", &["-h"], bin)?);
        let symbols = parse_symbols(&command_text("nm", &["-S", "--defined-only"], bin)?);
        // Fails on a binary with no dynamic section, which has no relocations.
        let relocs = parse_relocs(&command_text("objdump", &["-R"], bin).unwrap_or_default());
        let image = std::fs::read(bin).ok()?;
        let mut resolver = Resolver {
            sections: &sections,
            symbols: &symbols,
            relocs: &relocs,
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
        let mut stubs = HashMap::new();
        let hashes = hash_functions(&mut resolver, wanted, BufReader::new(stdout), &mut stubs);
        child
            .wait()
            .ok()?
            .success()
            .then_some(Binary { hashes, stubs })
    }

    #[cfg(test)]
    pub fn with_hashes(hashes: HashMap<String, u64>) -> Binary {
        Binary {
            hashes,
            stubs: HashMap::new(),
        }
    }

    /// `functions` with each symbolless address replaced by its stub name;
    /// `None` if one has no name here.
    pub fn name_stubs(&self, functions: Vec<(String, String)>) -> Option<Vec<(String, String)>> {
        functions
            .into_iter()
            .map(|(ob, sym)| {
                if ob == EXE && sym.starts_with("0x") {
                    Some((ob, format!("{STUB}{}", self.stubs.get(&sym)?)))
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
            } else if let Some(stub) = sym.strip_prefix(STUB) {
                writeln!(text, "p {stub}").ok()?;
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

    #[test]
    fn only_plt_labels_name_a_symbolless_address() {
        let (sections, symbols) = (parse_sections(SECTIONS), parse_symbols(SYMBOLS));
        let mut resolver = Resolver {
            sections: &sections,
            symbols: &symbols,
            relocs: &[],
            image: &[],
            roots: HashMap::new(),
        };
        let wanted: BTreeSet<String> = [
            "0x0000000000023eb0",
            "0x0000000000024000",
            "0x0000000000024100",
        ]
        .map(String::from)
        .into();
        let dis = "\
0000000000023eb0 <memcpy@plt>:
   23eb0:\tjmp    *0x1000(%rip)
0000000000024000 <_init>:
   24000:\tret
0000000000024100 <.text>:
   24100:\tret
";
        let mut stubs = HashMap::new();
        hash_functions(
            &mut resolver,
            &wanted,
            Cursor::new(dis.as_bytes()),
            &mut stubs,
        );
        assert_eq!(
            stubs.into_iter().collect::<Vec<_>>(),
            [("0x0000000000023eb0".to_string(), "memcpy@plt".to_string())]
        );
    }

    #[test]
    fn a_stub_address_becomes_its_name_and_hashes_by_it() {
        let mut binary = Binary::with_hashes(HashMap::new());
        binary
            .stubs
            .insert("0x0000000000023eb0".into(), "memcpy@plt".into());
        let named = binary
            .name_stubs(vec![(EXE.into(), "0x0000000000023eb0".into())])
            .expect("named");
        assert_eq!(named, [(EXE.to_string(), "plt:memcpy@plt".to_string())]);
        assert!(binary.code_key(&named).is_some());
        assert!(
            binary
                .name_stubs(vec![(EXE.into(), "0x0000000000000001".into())])
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
        let first = Binary::open(&exe, &wanted).unwrap().code_key(&functions);
        let second = Binary::open(&exe, &wanted).unwrap().code_key(&functions);
        assert!(first.is_some());
        assert_eq!(first, second);
    }
}
