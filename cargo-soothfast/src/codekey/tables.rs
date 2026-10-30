//! Jump tables decoded into the places in a function they jump to.

use std::collections::HashMap;

use soothfast_registry::fnv1a;

use super::elf::Target;
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

    /// Whether the relocated slot at `slot` holds a symbol outside this
    /// function, or the entry of another function of this image.
    pub(super) fn slot_leaves(&self, slot: u64, start: u64, end: u64) -> bool {
        let Ok(i) = self.relocs.binary_search_by_key(&slot, |r| r.offset) else {
            return false;
        };
        match &self.relocs[i].target {
            Target::Symbol(_) => true,
            Target::Relative(to) => self.symbol_at(*to).is_some_and(|s| {
                let s = &self.symbols[s];
                s.addr == *to && !(start..end).contains(to) && !s.name.contains(".cold")
            }),
        }
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
        let mut targets = HashMap::new();
        for (j, bases) in jumptable::group(tables.iter().copied(), |t| self.table_input(t)) {
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
