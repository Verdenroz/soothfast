//! Addresses turned into names and content that do not move with the layout.

use std::collections::HashMap;

use soothfast_registry::fnv1a;

use super::elf::{Marker, Reloc, Section, Symbol, Target};
use super::strip_llvm;
use crate::jumptable;
use crate::linkmap::Extent;

/// A piece of data a walk can enter.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Node {
    Symbol(usize),
    Section(usize),
    /// An input section the link map names, inside an output section.
    Input(usize),
    /// One entry of a fixed-size constant pool, by start and size.
    Entry(u64, u64),
}

/// What an address resolves to.
pub(super) enum Place {
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
pub(super) struct Resolver<'a> {
    pub(super) sections: &'a [Section],
    pub(super) symbols: &'a [Symbol],
    pub(super) relocs: &'a [Reloc],
    pub(super) markers: &'a HashMap<u64, Marker>,
    pub(super) inputs: &'a [Extent],
    pub(super) image: &'a [u8],
    pub(super) roots: HashMap<Node, Option<u64>>,
}

impl<'a> Resolver<'a> {
    pub(super) fn new(
        sections: &'a [Section],
        symbols: &'a [Symbol],
        relocs: &'a [Reloc],
        markers: &'a HashMap<u64, Marker>,
        inputs: &'a [Extent],
        image: &'a [u8],
    ) -> Resolver<'a> {
        Resolver {
            sections,
            symbols,
            relocs,
            markers,
            inputs,
            image,
            roots: HashMap::new(),
        }
    }

    fn section_at(&self, addr: u64) -> Option<usize> {
        let after = self.sections.partition_point(|s| s.vma <= addr);
        let i = self.sections[..after].iter().rposition(|s| s.size > 0)?;
        (addr - self.sections[i].vma < self.sections[i].size).then_some(i)
    }

    pub(super) fn symbol_at(&self, addr: u64) -> Option<usize> {
        let i = self
            .symbols
            .partition_point(|s| s.addr <= addr)
            .checked_sub(1)?;
        let s = &self.symbols[i];
        (addr - s.addr < s.size).then_some(i)
    }

    fn marker_at(&self, addr: u64) -> Option<Place> {
        Some(match self.markers.get(&addr)? {
            Marker::Opaque(name) => Place::Code(name.clone()),
            Marker::End { name, section } => Place::Bounded(name.clone(), Node::Section(*section)),
        })
    }

    /// `hint` is the symbol objdump printed next to the address, used only
    /// for code that has no sized symbol (PLT stubs) and GOT slots.
    fn place(&self, addr: u64, hint: Option<&str>) -> Option<Place> {
        let Some(section) = self.section_at(addr) else {
            return self.marker_at(addr);
        };
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
        if let Some(place) = self.marker_at(addr) {
            return Some(place);
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

    pub(super) fn input_at(&self, addr: u64) -> Option<usize> {
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
        let contents = self.sections[section].contents;
        let mut bytes = if contents {
            self.read_in(section, addr, size)?.to_vec()
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
        for r in &self.relocs[first..last] {
            walk.out.extend(format!("@{:x}=", r.offset - addr).bytes());
            let (text, next) = match &r.target {
                Target::Symbol(name) => (format!("sym:{}", strip_llvm(name)), None),
                Target::Relative(to) => match self.place(*to, None)? {
                    Place::Code(name) => (name, None),
                    Place::Bounded(name, next) => (format!("{name}("), Some(next)),
                    Place::Data(next, off, _) => (format!("+{off:x}("), Some(next)),
                },
            };
            walk.out.extend(text.bytes());
            if let Some(next) = next {
                self.walk(next, walk)?;
                walk.out.push(b')');
            }
            walk.out.push(b';');
        }
        Some(())
    }

    /// Bytes of `size` at `addr` in the file image, for a section with contents.
    pub(super) fn read(&self, addr: u64, size: u64) -> Option<&[u8]> {
        self.read_in(self.section_at(addr)?, addr, size)
    }

    /// Bytes of `size` at `addr`, read as part of `section`.
    fn read_in(&self, section: usize, addr: u64, size: u64) -> Option<&[u8]> {
        let s = &self.sections[section];
        if !s.contents {
            return None;
        }
        let start = usize::try_from(s.offset.checked_add(addr.checked_sub(s.vma)?)?).ok()?;
        self.image
            .get(start..start.checked_add(usize::try_from(size).ok()?)?)
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
        // A GOT entry reads as its relocation target. Elsewhere a relocated
        // address starts an object (a vtable) read past its first slot.
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
    pub(super) fn normalize(&mut self, body: &str) -> Option<String> {
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
            && is_transfer(mnemonic)
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

/// A jump or a call, whose direct target is an address to name.
fn is_transfer(mnemonic: &str) -> bool {
    jumptable::is_jump(mnemonic) || mnemonic.starts_with("call")
}

/// `3f000 <sym+0x10>` or a bare `3f000`.
pub(super) fn split_target(tail: &str) -> Option<(u64, Option<&str>)> {
    let addr = u64::from_str_radix(tail.split_whitespace().next()?, 16).ok()?;
    let hint = tail
        .find('<')
        .and_then(|i| tail.rfind('>').map(|j| &tail[i + 1..j]));
    Some((addr, hint))
}

/// `lea    0x90d6(%rip),%rsi        ` becomes `lea    (%rip),%rsi        `.
pub(super) fn strip_rip_displacement(operands: &str) -> String {
    let Some(at) = operands.find("(%rip)") else {
        return operands.to_string();
    };
    let start = operands[..at]
        .rfind(|c: char| !(c.is_ascii_hexdigit() || c == 'x' || c == '-'))
        .map_or(0, |i| i + 1);
    format!("{}{}", &operands[..start], &operands[at..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codekey::elf::parse_markers;
    use crate::codekey::fixture::{BAR, BAZ, DIS, FN_SYMBOLS, Fixture, bar, image, loaded};

    #[test]
    fn a_reference_into_unsized_data_hashes_its_section() {
        let dis = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tlea    0x1000(%rip),%rax        # 2010 <unsized>
    1007:\tret
";
        let fixture = Fixture::default();
        let a = bar(&fixture.hashes(&image(7, 1), dis));
        let b = bar(&fixture.hashes(&image(7, 2), dis));
        assert!(a.is_some());
        assert_ne!(a, b);
    }

    #[test]
    fn a_zero_sized_static_does_not_hide_the_object_at_its_address() {
        let markers = HashMap::from([(0x2000, Marker::Opaque("zst:_RNvCs1_3foo5EMPTY".into()))]);
        let fixture = Fixture::default().wanted(&[BAR]).markers(markers);
        let hash = |image: &[u8]| bar(&fixture.hashes(image, DIS));
        assert!(hash(&image(7, 1)).is_some());
        assert_ne!(hash(&image(7, 1)), hash(&image(8, 1)));
    }

    #[test]
    fn a_table_hashes_every_slot_not_just_the_first() {
        let symbols = crate::codekey::fixture::SYMBOLS
            .replace("0000000000000008 r anon", "0000000000000010 r anon");
        let relocs = |second: u64| {
            format!(
                "0000000000002000 R_X86_64_RELATIVE  *ABS*+0x0000000000001000\n\
                 0000000000002008 R_X86_64_RELATIVE  *ABS*+0x{second:016x}"
            )
        };
        let at = |second: u64| {
            let fixture = Fixture::default().symbols(&symbols).relocs(&relocs(second));
            bar(&fixture.hashes(&image(7, 1), DIS))
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
            bar(&Fixture::default().relocs(&relocs).hashes(&image, DIS))
        };
        assert!(at(0x1000, 0).is_some());
        assert_ne!(at(0x1000, 0), at(0x1040, 0));
        assert_eq!(at(0x1000, 0), at(0x1000, 9));
    }

    #[test]
    fn a_symbolic_relocation_hashes_by_its_name() {
        let with = |name: &str| {
            let relocs = format!("0000000000002000 R_X86_64_GLOB_DAT  {name}");
            bar(&Fixture::default().relocs(&relocs).hashes(&image(7, 1), DIS))
        };
        assert_ne!(with("alpha"), with("beta"));
        assert_eq!(with("alpha"), with("alpha"));
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

    fn cycle(symbols: &str, image: &[u8], dis: &str) -> (Option<u64>, Option<u64>) {
        let fixture = Fixture::default().symbols(symbols).relocs(CYCLE_RELOCS);
        let h = fixture.hashes(image, dis);
        (bar(&h), h.get(BAZ).copied())
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
        let (bar_a, baz_a) = cycle(CYCLE_SYMBOLS, &base, CYCLE_DIS);
        assert!(bar_a.is_some() && baz_a.is_some());
        assert_eq!((bar_a, baz_a), cycle(&swapped_symbols, &base, swapped));
        let (bar_b, baz_b) = cycle(CYCLE_SYMBOLS, &moved, CYCLE_DIS);
        assert_ne!(bar_a, bar_b);
        assert_ne!(baz_a, baz_b);
    }

    const UNNAMED: &str = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tlea    0x1000(%rip),%rax        # 2008 <anon>
    1007:\tret
";

    #[test]
    fn a_link_map_extent_bounds_a_reference_into_unnamed_data() {
        let hash = |inputs: &[Extent], byte: usize| {
            let mut image = image(7, 1);
            image[byte] = 9;
            let fixture = Fixture::default().wanted(&[BAR]).inputs(inputs);
            bar(&fixture.hashes(&image, UNNAMED))
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
        let pool = [Extent {
            vma: 0x2008,
            size: 16,
            entry: 8,
        }];
        let fixture = Fixture::default().wanted(&[BAR]).inputs(&pool);
        let hash = |byte: usize| {
            let mut image = image(7, 1);
            image[byte] = 9;
            bar(&fixture.hashes(&image, UNNAMED))
        };
        assert_eq!(hash(0x2010), hash(0x2017));
        assert_ne!(hash(0x2008), hash(0x2009));
    }

    #[test]
    fn a_stop_symbol_hashes_the_section_it_ends_and_not_the_one_after() {
        let sections = "\
Idx Name          Size      VMA               LMA               File off  Algn
  0 .text         00000100  0000000000001000  0000000000001000  00001000  2**4
                  CONTENTS, ALLOC, LOAD, READONLY, CODE
  1 .got          00000008  0000000000002000  0000000000002000  00002000  2**3
                  CONTENTS, ALLOC, LOAD, DATA
  2 linkme_x      00000008  0000000000002008  0000000000002008  00002008  2**3
                  CONTENTS, ALLOC, LOAD, DATA
  3 .after        00000020  0000000000002010  0000000000002010  00002010  2**3
                  CONTENTS, ALLOC, LOAD, DATA
";
        let dis = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tcmp    0x1000(%rip),%rax        # 2000 <_GLOBAL_OFFSET_TABLE_>
    1007:\tret
";
        let fixture = |markers| {
            Fixture::default()
                .sections(sections)
                .symbols(FN_SYMBOLS)
                .relocs("0000000000002000 R_X86_64_RELATIVE  *ABS*+0x0000000000002010")
                .wanted(&[BAR])
                .markers(markers)
        };
        let hash = |fixture: &Fixture, at: usize, byte: u8| {
            let mut image = vec![0; 0x3000];
            image[at] = byte;
            bar(&fixture.hashes(&image, dis))
        };
        let stops = fixture(parse_markers(
            "0000000000002010 r __stop_linkme_x\n",
            &loaded(sections),
        ));
        assert!(hash(&stops, 0x2018, 1).is_some());
        assert_eq!(hash(&stops, 0x2018, 1), hash(&stops, 0x2018, 2));
        assert_ne!(hash(&stops, 0x2009, 1), hash(&stops, 0x2009, 2));
        let none = fixture(HashMap::new());
        assert_ne!(hash(&none, 0x2018, 1), hash(&none, 0x2018, 2));
    }

    #[test]
    fn a_linker_boundary_symbol_names_an_address_outside_every_section() {
        let sections = "\
Idx Name          Size      VMA               LMA               File off  Algn
  0 .text         00000100  0000000000001000  0000000000001000  00001000  2**4
                  CONTENTS, ALLOC, LOAD, READONLY, CODE
  1 .got.plt      00000018  0000000000002000  0000000000002000  00002000  2**3
                  CONTENTS, ALLOC, LOAD, DATA
  2 .tm_clone_table 00000000  0000000000002018  0000000000002018  00002018  2**3
                  CONTENTS, ALLOC, LOAD, DATA
  3 .data         00000008  0000000000002020  0000000000002020  00002020  2**3
                  CONTENTS, ALLOC, LOAD, DATA
";
        let dis = |end: &str| {
            format!(
                "0000000000001000 <_ZN3foo3bar17h1E>:\n    1000:\tlea    0x1000(%rip),%rax        # 2018 <{end}>\n    1007:\tret\n"
            )
        };
        let at = |listing: &str, name: &str| {
            let markers = parse_markers(listing, &loaded(sections));
            let fixture = Fixture::default()
                .sections(sections)
                .symbols(FN_SYMBOLS)
                .wanted(&[BAR])
                .markers(markers);
            bar(&fixture.hashes(&image(7, 1), &dis(name)))
        };
        let named = "0000000000002018 D __TMC_END__\n";
        assert!(at(named, "__TMC_END__").is_some());
        assert_ne!(
            at(named, "__TMC_END__"),
            at("0000000000002018 B _end\n", "_end")
        );
        assert_eq!(at("", "nothing"), None);
    }

    #[test]
    fn an_unresolvable_reference_leaves_the_function_unhashed() {
        let dis = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tlea    0x1000(%rip),%rax        # 9000 <elsewhere>
";
        assert_eq!(bar(&Fixture::default().hashes(&image(7, 1), dis)), None);
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
}
