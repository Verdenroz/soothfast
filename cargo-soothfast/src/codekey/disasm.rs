//! Functions hashed off an `objdump -d` stream.

use std::collections::{BTreeSet, HashMap};
use std::io::BufRead;

use soothfast_registry::fnv1a;

use super::exits::FunctionExits;
use super::resolve::Resolver;
use super::tables::table_operand;
use super::{LABELED, is_rust_symbol, strip_llvm};
use crate::jumptable;

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
    fn new(start: u64, end: u64, name: String) -> Span {
        Span {
            start,
            end,
            names: vec![name],
            instrs: Vec::new(),
            text: String::new(),
            resolved: true,
        }
    }

    /// Normalize the instructions. A jump table the instructions prove (see
    /// `jumptable`) is hashed as the places in this function it jumps to.
    fn read(&mut self, resolver: &mut Resolver) {
        let proven = jumptable::prove(
            &self.instrs,
            (self.start, self.end),
            &|tables| resolver.table_targets(tables, self.start, self.end),
            &|tables| resolver.pointer_targets(tables),
            &FunctionExits {
                resolver,
                span: (self.start, self.end),
                rust: self.names.first().is_some_and(|n| is_rust_symbol(n)),
            },
        );
        let mut tables = resolver.decode_tables(&proven, self.start);
        tables.extend(resolver.decode_pointers(&proven));
        self.normalize(resolver, &tables);
    }

    /// Normalize the instructions, each `lea` in `tables` as its table.
    fn normalize(&mut self, resolver: &mut Resolver, tables: &HashMap<u64, (u64, u64)>) {
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

    /// `None` when an instruction did not resolve or there were none.
    fn hash(&self) -> Option<u64> {
        (self.resolved && !self.text.is_empty()).then(|| fnv1a(self.text.as_bytes()))
    }
}

/// Normalized hash per requested function, read off an `objdump -d` stream.
/// Copies of one name (per-CGU instances) hash together, so any copy changing
/// changes the name. A name with an unresolvable copy is left out.
///
/// Code named by address (`0x...`) or by an `at:` label has no extent in the
/// symbol table; it runs from its objdump label to the next one. `labels`
/// receives the `at:` name of each wanted address.
pub(super) fn hash_functions(
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
            None => spans.push(Span::new(s.addr, s.addr + s.size, name.to_string())),
        }
    }
    spans.sort_by_key(|s| s.start);

    let mut current: Option<usize> = None;
    let mut labeled: Vec<Span> = Vec::new();
    let mut open: Option<Span> = None;
    let mut line = Vec::new();
    while matches!(disassembly.read_until(b'\n', &mut line), Ok(n) if n > 0) {
        let text = String::from_utf8_lossy(&line);
        if let Some((addr, name)) = text
            .trim_end()
            .strip_suffix(">:")
            .and_then(|t| t.split_once(" <"))
            && let Ok(addr) = u64::from_str_radix(addr, 16)
        {
            labeled.extend(open.take().map(|span| Span { end: addr, ..span }));
            let hex = format!("0x{addr:016x}");
            let key = format!("{LABELED}{name}");
            if !name.starts_with('.') && (wanted.contains(&hex) || wanted.contains(&key)) {
                if wanted.contains(&hex) {
                    labels.insert(hex, key.clone());
                }
                open = Some(Span::new(addr, u64::MAX, key));
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
            if let Some(span) = &mut open {
                span.instrs.push((addr, body.trim().to_string()));
            }
        }
        line.clear();
    }
    labeled.extend(open);
    for span in &mut spans {
        span.read(resolver);
    }
    // Labeled code has no symbol extent to prove a jump table within.
    for span in &mut labeled {
        span.normalize(resolver, &HashMap::new());
    }
    let mut copies: HashMap<String, Vec<Option<u64>>> = HashMap::new();
    for span in spans.into_iter().chain(labeled) {
        let hash = span.hash();
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

#[cfg(test)]
mod tests {
    use crate::codekey::fixture::{DIS, Fixture, SECTIONS, SYMBOLS, bar, image};

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
        let base = Fixture::default().hashes(&image(7, 1), DIS);
        let layout = Fixture::default()
            .sections(&moved_sections)
            .symbols(&moved_symbols)
            .hashes(&image(7, 1), moved);
        assert!(bar(&base).is_some());
        assert_eq!(bar(&base), bar(&layout));
    }

    #[test]
    fn a_changed_constant_or_opcode_changes_the_hash() {
        let fixture = Fixture::default();
        let base = bar(&fixture.hashes(&image(7, 1), DIS));
        let constant = bar(&fixture.hashes(&image(8, 1), DIS));
        let opcode = bar(&fixture.hashes(&image(7, 1), &DIS.replace("ret", "hlt")));
        assert_ne!(base, constant);
        assert_ne!(base, opcode);
    }

    #[test]
    fn code_named_by_address_hashes_by_content_under_its_label() {
        let dis = |insn: &str| {
            format!(
                "0000000000024000 <_init>:\n   24000:\t{insn}\n0000000000024100 <.text>:\n   24100:\tret\n"
            )
        };
        let fixture = Fixture::default().wanted(&["0x0000000000024000", "0x0000000000024100"]);
        let (ret, labels) = fixture.labeled(&[], &dis("ret"));
        let (hlt, _) = fixture.labeled(&[], &dis("hlt"));
        assert_eq!(
            labels.into_iter().collect::<Vec<_>>(),
            [("0x0000000000024000".to_string(), "at:_init".to_string())]
        );
        assert_ne!(ret["at:_init"], hlt["at:_init"]);
        assert!(!ret.contains_key("at:.text"));
    }
}
