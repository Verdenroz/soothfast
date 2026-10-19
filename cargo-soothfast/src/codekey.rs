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

/// `.llvm.N` suffixes are per-build; the rest of the name is the identity.
fn strip_llvm(name: &str) -> &str {
    name.split(".llvm.").next().unwrap_or(name)
}

/// `functions` as a stored entry keeps them: the object that is `bin` named
/// [`EXE`], `.llvm.N` suffixes dropped, sorted and deduplicated.
pub fn portable(bin: &Path, functions: &[(String, String)]) -> Vec<(String, String)> {
    let canonical = bin.canonicalize().ok();
    let is_bin = |ob: &str| Path::new(ob) == bin || canonical.as_deref() == Some(Path::new(ob));
    let set: BTreeSet<(String, String)> = functions
        .iter()
        .map(|(ob, sym)| {
            let ob = if is_bin(ob) { EXE } else { ob };
            (ob.to_string(), strip_llvm(sym).to_string())
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

/// Turns an address into something that does not move with the layout.
struct Resolver<'a> {
    sections: &'a [Section],
    symbols: &'a [Symbol],
    relocs: &'a [Reloc],
    image: &'a [u8],
    symbol_hashes: HashMap<usize, Option<u64>>,
    section_hashes: HashMap<usize, Option<u64>>,
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

    /// Hash of a data range. A relocated slot holds a layout-dependent value
    /// (or zero) in the file, so its bytes are blanked and the target it
    /// resolves to is hashed in their place.
    fn bytes_hash(&mut self, section: usize, addr: u64, size: u64) -> Option<u64> {
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
        for i in first..last {
            let (slot, target) = (self.relocs[i].offset - addr, self.relocs[i].target.clone());
            let target = match target {
                Target::Relative(to) => self.resolve(to, None)?,
                Target::Symbol(name) => format!("sym:{}", strip_llvm(&name)),
            };
            if contents {
                let slot = slot as usize;
                let end = (slot + 8).min(bytes.len());
                bytes[slot..end].fill(0);
            }
            bytes.extend(format!("@{slot:x}={target}\n").bytes());
        }
        Some(fnv1a(&bytes))
    }

    fn symbol_hash(&mut self, symbol: usize, section: usize) -> Option<u64> {
        if let Some(h) = self.symbol_hashes.get(&symbol) {
            return *h;
        }
        // A pointer cycle reaches itself while hashing: the placeholder ends it.
        self.symbol_hashes.insert(symbol, Some(0));
        let (addr, size) = (self.symbols[symbol].addr, self.symbols[symbol].size);
        let h = self.bytes_hash(section, addr, size);
        self.symbol_hashes.insert(symbol, h);
        h
    }

    fn section_hash(&mut self, section: usize) -> Option<u64> {
        if let Some(h) = self.section_hashes.get(&section) {
            return *h;
        }
        self.section_hashes.insert(section, Some(0));
        let (vma, size) = (self.sections[section].vma, self.sections[section].size);
        let h = self.bytes_hash(section, vma, size);
        self.section_hashes.insert(section, h);
        h
    }

    /// `hint` is the symbol objdump printed next to the address, used only
    /// for code that has no sized symbol (PLT stubs).
    fn resolve(&mut self, addr: u64, hint: Option<&str>) -> Option<String> {
        let section = self.section_at(addr)?;
        let code = self.sections[section].code;
        if let Some(i) = self.symbol_at(addr)
            && self.section_at(self.symbols[i].addr) == Some(section)
        {
            let off = addr - self.symbols[i].addr;
            return Some(if code {
                format!("{}+{off:x}", strip_llvm(&self.symbols[i].name))
            } else {
                format!("data:{:016x}+{off:x}", self.symbol_hash(i, section)?)
            });
        }
        if code {
            return hint.map(|h| strip_llvm(h).to_string());
        }
        let off = addr - self.sections[section].vma;
        let slot = hint.filter(|h| h.ends_with("$got")).unwrap_or_default();
        Some(format!(
            "{}:{:016x}+{off:x}{slot}",
            self.sections[section].name,
            self.section_hash(section)?
        ))
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
) -> HashMap<String, u64> {
    let mut spans: Vec<Span> = Vec::new();
    for s in resolver.symbols {
        let name = strip_llvm(&s.name);
        if !wanted.contains(name) {
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
        if let Some((addr, body)) = text.split_once(':')
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
            symbol_hashes: HashMap::new(),
            section_hashes: HashMap::new(),
        };
        let mut child = Command::new("objdump")
            .args(["-d", "--no-show-raw-insn"])
            .arg(bin)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let hashes = hash_functions(&mut resolver, wanted, BufReader::new(stdout));
        child.wait().ok()?.success().then_some(Binary { hashes })
    }

    /// The key of `functions` (as [`portable`] returns them) in this binary,
    /// or `None` if any is unhashed here or any foreign object is unreadable.
    pub fn code_key(&self, functions: &[(String, String)]) -> Option<String> {
        let mut text = String::new();
        for (ob, sym) in functions {
            if ob == EXE {
                writeln!(text, "x {sym} {:016x}", self.hashes.get(sym)?).ok()?;
            } else {
                writeln!(text, "f {ob} {:016x} {sym}", file_digest(ob)?).ok()?;
            }
        }
        Some(format!("{:016x}", fnv1a(text.as_bytes())))
    }
}

/// Content digest of a foreign object, read once per path.
fn file_digest(path: &str) -> Option<u64> {
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
            symbol_hashes: HashMap::new(),
            section_hashes: HashMap::new(),
        };
        let wanted = ["_ZN3foo3bar17h1E".to_string()].into();
        hash_functions(&mut resolver, &wanted, Cursor::new(dis.as_bytes()))
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
    fn portable_names_the_bench_binary_and_drops_llvm_suffixes() {
        let functions = vec![
            ("/lib/libc.so.6".to_string(), "memcpy".to_string()),
            ("/w/target/bench".to_string(), "b.llvm.9".to_string()),
            ("/w/target/bench".to_string(), "a".to_string()),
        ];
        assert_eq!(
            portable(Path::new("/w/target/bench"), &functions),
            [
                ("/lib/libc.so.6".to_string(), "memcpy".to_string()),
                (EXE.to_string(), "a".to_string()),
                (EXE.to_string(), "b".to_string()),
            ]
        );
    }

    #[test]
    fn a_function_missing_from_the_binary_has_no_key() {
        let binary = Binary {
            hashes: HashMap::new(),
        };
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
