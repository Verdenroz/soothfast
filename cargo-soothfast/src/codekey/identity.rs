//! What tells two objects with the same bytes apart, and what a table word
//! names.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use super::elf::{Section, Symbol, in_data};
use super::resolve::{Node, Place, Resolver};
use super::strip_llvm;

/// The names that more than one data symbol or label of the binary has. Code
/// symbols do not count, so a new function cannot change what a static is
/// called. A new static of the same name elsewhere makes both duplicates:
/// that costs a miss until the link map names them, never a wrong hit.
pub(super) fn duplicated(
    sections: &[Section],
    symbols: &[Symbol],
    labels: &HashMap<u64, Vec<String>>,
) -> HashSet<String> {
    let mut counts: HashMap<Cow<str>, usize> = HashMap::new();
    let data = symbols
        .iter()
        .filter(|s| in_data(sections, s.addr))
        .map(|s| strip_llvm(&s.name));
    let unsized_names = labels.values().flatten().map(|n| Cow::Borrowed(n.as_str()));
    for name in data.chain(unsized_names) {
        *counts.entry(name).or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(name, _)| name.into_owned())
        .collect()
}

impl Resolver<'_> {
    /// What tells `node` from another object with the same bytes, when
    /// reading either gives different results: writable data is a place, so
    /// two zeroed statics are two objects. Read-only data is its bytes. A
    /// symbol is its own name unless another symbol of the binary has it too
    /// (statics of two files), when the input section it is defined in tells
    /// them apart. Data in a section with no symbol or input around it is
    /// named by the unique labels at its address, and only then.
    pub(super) fn identity(&self, node: Node, off: u64) -> Option<String> {
        let (section, _, size) = self.extent(node)?;
        if !self.sections[section].writable || size == 0 {
            return Some(String::new());
        }
        let name = match node {
            Node::Symbol(i) => {
                let s = &self.symbols[i];
                let name = strip_llvm(&s.name);
                if self.duplicated.contains(name.as_ref()) {
                    format!("{name}@{}", self.inputs[self.input_at(s.addr)?].name)
                } else {
                    name.into_owned()
                }
            }
            Node::Input(i) => self.inputs[i].name.clone(),
            Node::Section(i) => self.labelled(self.sections[i].vma + off),
            Node::Entry(..) => return None,
        };
        (!name.is_empty()).then(|| format!("@{name}"))
    }

    /// The names of the labels at `addr` that no other symbol shares.
    fn labelled(&self, addr: u64) -> String {
        let mut names: Vec<&str> = self
            .labels
            .get(&addr)
            .into_iter()
            .flatten()
            .map(String::as_str)
            .filter(|n| !self.duplicated.contains(*n))
            .collect();
        names.sort_unstable();
        names.join(",")
    }

    /// Where `addr` is if it lands strictly inside a sized symbol or a
    /// link-map input, and not past the end of one, on a marker or only on
    /// the section around it.
    pub(super) fn object_at(&self, addr: u64) -> Option<Place> {
        let section = self.section_at(addr)?;
        let named = self.in_symbol(addr, section).is_some();
        let inside = self.input_at(addr).is_some() && !self.sections[section].code;
        if named || (inside && !self.markers.contains_key(&addr)) {
            self.place(addr, None)
        } else {
            None
        }
    }

    /// What a table word naming `addr` hashes as, worked out once.
    pub(super) fn described(&self, addr: u64) -> Option<String> {
        if let Some(text) = self.described.borrow().get(&addr) {
            return text.clone();
        }
        let text = self.object_at(addr).and_then(|p| self.show(p));
        self.described.borrow_mut().insert(addr, text.clone());
        text
    }
}

#[cfg(test)]
mod tests {
    use crate::codekey::fixture::{
        BAR, FN_SYMBOLS, Fixture, OBJECTS, READ_ONLY, WRITABLE, bar, object_sections,
    };
    use crate::linkmap::Extent;

    fn takes(flags: &str, object: &str, relocs: &str) -> Option<u64> {
        let dis = format!(
            "0000000000001000 <_ZN3foo3bar17h1E>:\n    1000:\tlea    0x1000(%rip),%rax        # {object}\n    1007:\tret\n"
        );
        let fixture = Fixture::default()
            .sections(&object_sections(flags))
            .symbols(&format!(
                "{OBJECTS}0000000000002000 0000000000000008 r slot\n"
            ))
            .relocs(relocs)
            .wanted(&[BAR]);
        bar(&fixture.hashes(&vec![0; 0x4000], &dis))
    }

    #[test]
    fn two_writable_objects_with_the_same_bytes_hash_apart() {
        assert!(takes(WRITABLE, "3000 <one>", "").is_some());
        assert_ne!(
            takes(WRITABLE, "3000 <one>", ""),
            takes(WRITABLE, "3008 <two>", "")
        );
    }

    #[test]
    fn local_symbols_of_one_name_in_two_objects_hash_apart() {
        let symbols = "\
0000000000001000 0000000000000020 T _ZN3foo3bar17h1E
0000000000003000 0000000000000008 d counter
0000000000003008 0000000000000008 d counter
";
        let inputs = |first: &str, second: &str| {
            [(0x3000, first), (0x3008, second)].map(|(vma, name)| Extent {
                vma,
                size: 8,
                name: name.to_string(),
                ..Extent::default()
            })
        };
        let takes = |inputs: &[Extent], object: &str| {
            let dis = format!(
                "0000000000001000 <_ZN3foo3bar17h1E>:\n    1000:\tlea    0x1000(%rip),%rax        # {object} <counter>\n    1007:\tret\n"
            );
            let fixture = Fixture::default()
                .sections(&object_sections(WRITABLE))
                .symbols(symbols)
                .inputs(inputs)
                .wanted(&[BAR]);
            bar(&fixture.hashes(&vec![0; 0x4000], &dis))
        };
        let apart = inputs("a.o:.data.counter", "b.o:.data.counter");
        assert!(takes(&apart, "3000").is_some());
        assert_ne!(takes(&apart, "3000"), takes(&apart, "3008"));
        assert_eq!(takes(&[], "3000"), None);
    }

    #[test]
    fn a_local_symbol_with_a_name_of_its_own_needs_no_input() {
        let symbols = "\
0000000000001000 0000000000000020 T _ZN3foo3bar17h1E
0000000000003000 0000000000000008 d completed.0
0000000000003008 0000000000000008 d solo
";
        let takes = |object: &str| {
            let dis = format!(
                "0000000000001000 <_ZN3foo3bar17h1E>:\n    1000:\tlea    0x1000(%rip),%rax        # {object}\n    1007:\tret\n"
            );
            let fixture = Fixture::default()
                .sections(&object_sections(WRITABLE))
                .symbols(symbols)
                .wanted(&[BAR]);
            bar(&fixture.hashes(&vec![0; 0x4000], &dis))
        };
        assert!(takes("3000 <a>").is_some());
        assert_ne!(takes("3000 <a>"), takes("3008 <b>"));
    }

    #[test]
    fn a_unique_label_names_the_writable_data_at_its_address() {
        let takes = |object: &str, labels: &str| {
            let dis = format!(
                "0000000000001000 <_ZN3foo3bar17h1E>:\n    1000:\tlea    0x1000(%rip),%rax        # {object}\n    1007:\tret\n"
            );
            let fixture = Fixture::default()
                .sections(&object_sections(WRITABLE))
                .symbols(FN_SYMBOLS)
                .labels(labels)
                .wanted(&[BAR]);
            bar(&fixture.hashes(&vec![0; 0x4000], &dis))
        };
        let labels = "\
0000000000003000 d __dso_handle
0000000000003000 d __alias
0000000000003008 d solo
";
        assert!(takes("3000 <__dso_handle>", labels).is_some());
        assert_ne!(takes("3000 <a>", labels), takes("3008 <b>", labels));
        assert_eq!(takes("3004 <inside>", labels), None);
        assert_eq!(takes("3000 <a>", ""), None);
        let twice = "0000000000003000 d same\n0000000000003008 d same\n";
        assert_eq!(takes("3000 <a>", twice), None);
        let one_twice =
            "0000000000003000 d same\n0000000000003000 d only\n0000000000003008 d same\n";
        assert!(takes("3000 <a>", one_twice).is_some());
    }

    #[test]
    fn a_writable_section_with_no_symbol_or_input_leaves_the_function_unhashed() {
        let takes = |flags: &str| {
            let dis = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tlea    0x1000(%rip),%rax        # 3004 <inside>
    1007:\tret
";
            let fixture = Fixture::default()
                .sections(&object_sections(flags))
                .symbols(FN_SYMBOLS)
                .wanted(&[BAR]);
            bar(&fixture.hashes(&vec![0; 0x4000], dis))
        };
        assert!(takes(READ_ONLY).is_some());
        assert_eq!(takes(WRITABLE), None);
    }

    #[test]
    fn two_read_only_objects_with_the_same_bytes_hash_alike() {
        assert!(takes(READ_ONLY, "3000 <one>", "").is_some());
        assert_eq!(
            takes(READ_ONLY, "3000 <one>", ""),
            takes(READ_ONLY, "3008 <two>", "")
        );
    }

    #[test]
    fn a_pointer_to_a_writable_object_hashes_which_object() {
        let slot = |to: u64| {
            let relocs = format!("0000000000002000 R_X86_64_RELATIVE  *ABS*+0x{to:016x}");
            takes(WRITABLE, "2000 <slot>", &relocs)
        };
        assert!(slot(0x3000).is_some());
        assert_ne!(slot(0x3000), slot(0x3008));
    }

    #[test]
    fn a_slot_pointing_at_writable_data_with_no_symbol_or_map_leaves_the_function_unhashed() {
        let slot = |symbols: &str| {
            let dis = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tlea    0x1000(%rip),%rax        # 2000 <slot>
    1007:\tret
";
            let fixture = Fixture::default()
                .sections(&object_sections(WRITABLE))
                .symbols(symbols)
                .relocs("0000000000002000 R_X86_64_RELATIVE  *ABS*+0x0000000000003004")
                .wanted(&[BAR]);
            bar(&fixture.hashes(&vec![0; 0x4000], dis))
        };
        let slot_only = "\
0000000000001000 0000000000000020 T _ZN3foo3bar17h1E
0000000000002000 0000000000000008 r slot
";
        assert_eq!(slot(slot_only), None);
        let named = format!("{slot_only}0000000000003000 0000000000000010 D object\n");
        assert!(slot(&named).is_some());
    }

    #[test]
    fn a_relro_section_is_read_only_and_one_outside_it_is_not() {
        let hash = |relro: Option<(u64, u64)>| {
            let dis = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tlea    0x1000(%rip),%rax        # 3004 <inside>
    1007:\tret
";
            let fixture = Fixture::default()
                .sections(&object_sections(WRITABLE))
                .symbols(FN_SYMBOLS)
                .wanted(&[BAR]);
            let fixture = match relro {
                Some((start, size)) => fixture.relro(start, size),
                None => fixture,
            };
            bar(&fixture.hashes(&vec![0; 0x4000], dis))
        };
        assert_eq!(hash(None), None);
        assert_eq!(hash(Some((0x3000, 0x10))), hash(Some((0x2000, 0x2000))));
        assert!(hash(Some((0x3000, 0x10))).is_some());
        assert_eq!(hash(Some((0x3000, 0x8))), None);
    }
}
