//! Jump tables decoded into the places in a function they jump to.

use std::collections::HashMap;

use soothfast_registry::fnv1a;

use super::resolve::{Resolver, strip_rip_displacement};
use crate::jumptable;

impl Resolver<'_> {
    /// For each proven `lea`, the hash of the table it takes and the offset
    /// into its section. A jump table decodes when its whole input section
    /// is words that, from the nearest proven base at or before them, land
    /// inside the function; a table of offsets to data, when each word names
    /// something to hash. `prove` keeps the words of every table that does.
    /// Anything else stays raw bytes.
    pub(super) fn decode_tables(
        &self,
        proven: &jumptable::Proven,
        start: u64,
    ) -> HashMap<u64, (u64, u64)> {
        let mut tables = self.decode(&proven.proofs, &proven.targets, |size, words| {
            Some(table_hash(size, words, start))
        });
        tables.extend(
            self.decode(&proven.pointers, &proven.targets, |size, words| {
                self.pointer_hash(size, words)
            }),
        );
        tables
    }

    fn decode(
        &self,
        proofs: &[jumptable::Proof],
        targets: &HashMap<u64, Vec<u64>>,
        hash: impl Fn(u64, &[u64]) -> Option<u64>,
    ) -> HashMap<u64, (u64, u64)> {
        let mut tables = HashMap::new();
        for (j, bases) in jumptable::group(proofs.iter().map(|p| p.table), |t| self.table_input(t))
        {
            let Some(words) = bases
                .iter()
                .map(|b| targets.get(b))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            let words: Vec<u64> = words.into_iter().flatten().copied().collect();
            let Some(hash) = hash(self.inputs[j].size, &words) else {
                continue;
            };
            for p in proofs
                .iter()
                .filter(|p| bases.binary_search(&p.table).is_ok())
            {
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
    pub(super) fn table_targets(
        &self,
        tables: &[u64],
        start: u64,
        end: u64,
    ) -> HashMap<u64, Vec<u64>> {
        self.targets_of(tables, |target| (start..end).contains(&target))
    }

    /// For each candidate table of offsets to data that decodes, the
    /// addresses it names, each of which resolves to something to hash.
    pub(super) fn pointer_targets(&self, tables: &[u64]) -> HashMap<u64, Vec<u64>> {
        self.targets_of(tables, |target| self.described(target).is_some())
    }

    fn targets_of(&self, tables: &[u64], lands: impl Fn(u64) -> bool) -> HashMap<u64, Vec<u64>> {
        let mut targets = HashMap::new();
        for (j, bases) in jumptable::group(tables.iter().copied(), |t| self.table_input(t)) {
            let Some(words) = self.decode_words(j, &bases, &lands) else {
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

    /// Hash of the places the words of a table of `size` bytes name.
    fn pointer_hash(&self, size: u64, words: &[u64]) -> Option<u64> {
        let mut text = format!("pointers {size}:");
        for &target in words {
            text.push_str(&self.described(target)?);
            text.push(',');
        }
        Some(fnv1a(text.as_bytes()))
    }

    /// The place each word of the input section names, measured from the
    /// nearest base at or before it. `None` unless every one satisfies
    /// `lands` and the section is nothing but such words.
    fn decode_words(
        &self,
        input: usize,
        bases: &[u64],
        lands: &impl Fn(u64) -> bool,
    ) -> Option<Vec<u64>> {
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
            if !lands(target) || (e.vma..e.vma + e.size).contains(&target) {
                return None;
            }
            targets.push(target);
        }
        Some(targets)
    }
}

/// Hash of the decoded words of a jump table of `size` bytes.
fn table_hash(size: u64, words: &[u64], start: u64) -> u64 {
    let mut text = format!("{size}:");
    for target in words {
        text.push_str(&format!("{:x},", target - start));
    }
    fnv1a(text.as_bytes())
}

/// `lea T(%rip),%rA` with the table in place of its address.
pub(super) fn table_operand(body: &str, hash: u64, off: u64) -> Option<String> {
    let (head, _) = body.rsplit_once("# ")?;
    Some(format!(
        "{}# table:{hash:016x}+{off:x}",
        strip_rip_displacement(head)
    ))
}

#[cfg(test)]
mod tests {
    use crate::codekey::elf::Marker;
    use crate::codekey::fixture::{
        BAR, FN_SYMBOLS, Fixture, OBJECTS, READ_ONLY, WRITABLE, bar, object_sections,
    };
    use crate::linkmap::Extent;
    use std::collections::HashMap;

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
        let inputs = [Extent {
            vma: extent.0,
            size: extent.1,
            entry: 0,
            ..Extent::default()
        }];
        let fixture = Fixture::default()
            .symbols(FN_SYMBOLS)
            .inputs(&inputs)
            .wanted(&[BAR]);
        bar(&fixture.hashes(image, dis))
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

    const NAMES: &str = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tmovzbl (%rdi),%ecx
    1003:\tlea    0x1ff6(%rip),%rdi        # 2000 <anon.0>
    100a:\tmovslq (%rdi,%rcx,4),%rsi
    100e:\tadd    %rdi,%rsi
    1011:\tret
";

    /// The offsets table at `0x2000` naming two strings placed at `strings`,
    /// and with `stray` a third word naming an address nothing covers. The
    /// strings are link-map inputs only when `mapped`.
    fn names(strings: [u64; 2], bytes: [u8; 2], stray: bool, mapped: bool) -> Option<u64> {
        let mut image = vec![0; 0x3000];
        let size = if stray { 12 } else { 8 };
        let mut inputs = vec![Extent {
            vma: 0x2000,
            size,
            ..Extent::default()
        }];
        for (i, (at, byte)) in strings.into_iter().zip(bytes).enumerate() {
            let word = (at - 0x2000) as i32;
            image[0x2000 + 4 * i..0x2004 + 4 * i].copy_from_slice(&word.to_le_bytes());
            image[at as usize..at as usize + 8].fill(byte);
            if mapped {
                inputs.push(Extent {
                    vma: at,
                    size: 8,
                    ..Extent::default()
                });
            }
        }
        if stray {
            image[0x2008..0x200c].copy_from_slice(&0x5000i32.to_le_bytes());
        }
        inputs.sort_by_key(|e| e.vma);
        let fixture = Fixture::default()
            .symbols(FN_SYMBOLS)
            .inputs(&inputs)
            .wanted(&[BAR]);
        bar(&fixture.hashes(&image, NAMES))
    }

    #[test]
    fn a_table_of_offsets_to_strings_hashes_the_strings_wherever_they_sit() {
        let base = names([0x2008, 0x2010], [1, 2], false, true);
        assert!(base.is_some());
        assert_eq!(base, names([0x2010, 0x2018], [1, 2], false, true));
        assert_ne!(base, names([0x2008, 0x2010], [1, 3], false, true));
        assert_ne!(base, names([0x2008, 0x2010], [2, 1], false, true));
    }

    #[test]
    fn a_table_with_a_word_naming_nothing_stays_raw() {
        let base = names([0x2010, 0x2018], [1, 2], true, true);
        assert!(base.is_some());
        assert_ne!(base, names([0x2018, 0x2010], [1, 2], true, true));
    }

    #[test]
    fn a_table_word_landing_on_the_table_itself_stays_raw() {
        let hash = |string: u64| {
            let mut image = vec![0; 0x3000];
            image[0x2000..0x2004].copy_from_slice(&((string - 0x2000) as i32).to_le_bytes());
            let inputs = [0x2000, string].map(|vma| Extent {
                vma,
                size: 8,
                ..Extent::default()
            });
            let fixture = Fixture::default()
                .symbols(FN_SYMBOLS)
                .inputs(&inputs)
                .wanted(&[BAR]);
            bar(&fixture.hashes(&image, NAMES))
        };
        assert!(hash(0x2010).is_some());
        assert_ne!(hash(0x2010), hash(0x2018));
    }

    /// A one-word table at `0x2000` naming the address `target`.
    fn one_word(target: u64, fixture: Fixture) -> Option<u64> {
        let mut image = vec![0; 0x3000];
        image[0x2000..0x2004].copy_from_slice(&((target - 0x2000) as i32).to_le_bytes());
        bar(&fixture.wanted(&[BAR]).hashes(&image, NAMES))
    }

    fn table_input() -> Extent {
        Extent {
            vma: 0x2000,
            size: 4,
            ..Extent::default()
        }
    }

    #[test]
    fn a_word_one_past_a_sized_symbol_keeps_the_table_raw() {
        let hash = |prev: u64| {
            let symbols = format!("{FN_SYMBOLS}{prev:016x} 0000000000000008 r prev\n");
            let fixture = Fixture::default()
                .symbols(&symbols)
                .inputs(&[table_input()]);
            one_word(prev + 8, fixture)
        };
        assert!(hash(0x2008).is_some());
        assert_ne!(hash(0x2008), hash(0x2010));
    }

    #[test]
    fn a_word_one_past_an_input_keeps_the_table_raw() {
        let hash = |prev: u64| {
            let prev = Extent {
                vma: prev,
                size: 8,
                ..Extent::default()
            };
            let fixture = Fixture::default()
                .symbols(FN_SYMBOLS)
                .inputs(&[table_input(), prev.clone()]);
            one_word(prev.vma + 8, fixture)
        };
        assert!(hash(0x2008).is_some());
        assert_ne!(hash(0x2008), hash(0x2010));
    }

    #[test]
    fn a_word_at_a_zero_sized_static_keeps_the_table_raw() {
        let hash = |at: u64| {
            let string = Extent {
                vma: at,
                size: 8,
                ..Extent::default()
            };
            let markers = HashMap::from([(at, Marker::Opaque("zst:Z".into()))]);
            let fixture = Fixture::default()
                .symbols(FN_SYMBOLS)
                .inputs(&[table_input(), string])
                .markers(markers);
            one_word(at, fixture)
        };
        assert!(hash(0x2008).is_some());
        assert_ne!(hash(0x2008), hash(0x2010));
    }

    #[test]
    fn a_table_naming_only_the_section_around_a_word_stays_raw() {
        let base = names([0x2010, 0x2018], [1, 2], false, false);
        assert!(base.is_some());
        assert_ne!(base, names([0x2018, 0x2010], [1, 2], false, false));
    }

    #[test]
    fn a_table_naming_writable_objects_hashes_which_object_each_word_names() {
        let hash = |flags: &str, second: u64| {
            let mut image = vec![0; 0x4000];
            for (i, at) in [0x3000u64, second].into_iter().enumerate() {
                let word = (at - 0x2000) as i32;
                image[0x2000 + 4 * i..0x2004 + 4 * i].copy_from_slice(&word.to_le_bytes());
            }
            let inputs = [Extent {
                vma: 0x2000,
                size: 8,
                ..Extent::default()
            }];
            let fixture = Fixture::default()
                .sections(&object_sections(flags))
                .symbols(OBJECTS)
                .inputs(&inputs)
                .wanted(&[BAR]);
            bar(&fixture.hashes(&image, NAMES))
        };
        assert!(hash(WRITABLE, 0x3008).is_some());
        assert_ne!(hash(WRITABLE, 0x3008), hash(WRITABLE, 0x3000));
        assert_eq!(hash(READ_ONLY, 0x3008), hash(READ_ONLY, 0x3000));
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
}
