//! Jump tables decoded into the places in a function they jump to.

use std::collections::HashMap;

use soothfast_registry::fnv1a;

use super::resolve::{Resolver, strip_rip_displacement};
use crate::jumptable;

impl Resolver<'_> {
    /// For each proven `lea`, the hash of the jump table it takes and the
    /// offset into its section. A table decodes when its whole input section
    /// is words that, from the nearest proven base at or before them, land
    /// inside the function; `prove` keeps the words of every table that does.
    /// Anything else stays raw bytes.
    pub(super) fn decode_tables(
        &self,
        proven: &jumptable::Proven,
        start: u64,
    ) -> HashMap<u64, (u64, u64)> {
        let table_input = |table: u64| self.table_input(table);
        let mut tables = HashMap::new();
        for (j, bases) in jumptable::group(proven.proofs.iter().map(|p| p.table), table_input) {
            let Some(words) = bases
                .iter()
                .map(|b| proven.targets.get(b))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            let hash = self.table_hash(j, words.into_iter().flatten(), start);
            for p in proven
                .proofs
                .iter()
                .filter(|p| table_input(p.table) == Some(j))
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

    /// For each proven `lea` of a table of offsets to data, the hash of the
    /// table and the offset into its section. The table is hashed as the data
    /// each word names, so it holds still while that data moves.
    pub(super) fn decode_pointers(
        &mut self,
        proven: &jumptable::Proven,
    ) -> HashMap<u64, (u64, u64)> {
        let mut tables = HashMap::new();
        let proofs = &proven.pointers;
        for (j, bases) in jumptable::group(proofs.iter().map(|p| p.table), |t| self.table_input(t))
        {
            let Some(words) = bases
                .iter()
                .map(|b| proven.targets.get(b))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            let Some(hash) = self.pointer_hash(j, words.into_iter().flatten()) else {
                continue;
            };
            for p in proofs
                .iter()
                .filter(|p| self.table_input(p.table) == Some(j))
            {
                tables.insert(p.lea, (hash, p.table - self.inputs[j].vma));
            }
        }
        tables
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
        self.targets_of(tables, |target| self.place(target, None).is_some())
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

    /// Hash of the decoded words of input section `input`.
    fn table_hash<'w>(
        &self,
        input: usize,
        words: impl Iterator<Item = &'w u64>,
        start: u64,
    ) -> u64 {
        let mut text = format!("{}:", self.inputs[input].size);
        for target in words {
            text.push_str(&format!("{:x},", target - start));
        }
        fnv1a(text.as_bytes())
    }

    /// Hash of the places the words of input section `input` name.
    fn pointer_hash<'w>(
        &mut self,
        input: usize,
        words: impl Iterator<Item = &'w u64>,
    ) -> Option<u64> {
        let mut text = format!("pointers {}:", self.inputs[input].size);
        for &target in words {
            text.push_str(&self.describe(target, None)?);
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
            if !lands(target) {
                return None;
            }
            targets.push(target);
        }
        Some(targets)
    }
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
    use crate::codekey::fixture::{BAR, FN_SYMBOLS, Fixture, bar};
    use crate::linkmap::Extent;

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

    /// The offsets table at `0x2000` naming two strings, placed at `strings`.
    fn names(strings: [u64; 2], bytes: [u8; 2]) -> Option<u64> {
        let mut image = vec![0; 0x3000];
        let mut inputs = vec![Extent {
            vma: 0x2000,
            size: 8,
            entry: 0,
        }];
        for (i, (at, byte)) in strings.into_iter().zip(bytes).enumerate() {
            let word = (at - 0x2000) as i32;
            image[0x2000 + 4 * i..0x2004 + 4 * i].copy_from_slice(&word.to_le_bytes());
            image[at as usize..at as usize + 8].fill(byte);
            inputs.push(Extent {
                vma: at,
                size: 8,
                entry: 0,
            });
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
        let base = names([0x2008, 0x2010], [1, 2]);
        assert!(base.is_some());
        assert_eq!(base, names([0x2010, 0x2018], [1, 2]));
        assert_ne!(base, names([0x2008, 0x2010], [1, 3]));
        assert_ne!(base, names([0x2008, 0x2010], [2, 1]));
    }

    #[test]
    fn a_table_naming_an_unknown_place_stays_raw() {
        let hash = |word: i32| {
            let mut image = vec![0; 0x3000];
            image[0x2000..0x2004].copy_from_slice(&word.to_le_bytes());
            let inputs = [Extent {
                vma: 0x2000,
                size: 4,
                entry: 0,
            }];
            let fixture = Fixture::default()
                .symbols(FN_SYMBOLS)
                .inputs(&inputs)
                .wanted(&[BAR]);
            bar(&fixture.hashes(&image, NAMES))
        };
        assert_ne!(hash(0x1000), hash(0x2000));
        assert_ne!(hash(0x7000), hash(0x7010));
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
