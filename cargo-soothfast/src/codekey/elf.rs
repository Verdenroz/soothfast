//! What `objdump` and `nm` say about a bench binary: its sections, sized
//! symbols, dynamic relocations, and the addresses that name no data.

use std::collections::HashMap;

use soothfast_registry::fnv1a;

use super::strip_llvm;

pub(super) struct Section {
    pub(super) name: String,
    pub(super) vma: u64,
    pub(super) size: u64,
    pub(super) offset: u64,
    pub(super) code: bool,
    pub(super) contents: bool,
    pub(super) tls: bool,
    /// Data the program can change after the loader is done with it.
    pub(super) writable: bool,
}

impl Section {
    /// Whether `addr` falls inside this section. An empty one contains nothing.
    pub(super) fn contains(&self, addr: u64) -> bool {
        addr >= self.vma && addr - self.vma < self.size
    }
}

pub(super) struct Symbol {
    pub(super) addr: u64,
    pub(super) size: u64,
    pub(super) name: String,
    /// Code, by `nm`'s type letter (`T`, `t`, `W`, `w`).
    pub(super) func: bool,
}

/// One section of an `objdump -h` listing.
pub(crate) struct SectionHeader<'a> {
    pub name: &'a str,
    pub size: u64,
    pub vma: u64,
    pub offset: u64,
    pub flags: &'a str,
}

/// The sections of an `objdump -h` listing, each an `Idx Name Size VMA LMA
/// File-off Algn` line followed by a line of flags.
pub(crate) fn section_headers(listing: &str) -> impl Iterator<Item = SectionHeader<'_>> {
    let mut lines = listing.lines();
    std::iter::from_fn(move || {
        while let Some(header) = lines.next() {
            let f: Vec<&str> = header.split_whitespace().collect();
            if f.len() < 7 || f[0].parse::<u32>().is_err() {
                continue;
            }
            let flags = lines.next()?;
            let hex = |s: &str| u64::from_str_radix(s, 16).ok();
            if let (Some(size), Some(vma), Some(offset)) = (hex(f[2]), hex(f[3]), hex(f[5])) {
                return Some(SectionHeader {
                    name: f[1],
                    size,
                    vma,
                    offset,
                    flags,
                });
            }
        }
        None
    })
}

/// Digest of the thread-local initial image (`.tdata` contents, `.tbss`
/// sizes). Code reaches it through `%fs` offsets, which name no address.
pub(super) fn tls_digest(sections: &[Section], image: &[u8]) -> Option<u64> {
    let mut text = Vec::new();
    for s in sections.iter().filter(|s| s.tls) {
        text.extend(format!("{}:{}:", s.name, s.size).bytes());
        if s.contents {
            let offset = usize::try_from(s.offset).ok()?;
            text.extend(image.get(offset..offset.checked_add(usize::try_from(s.size).ok()?)?)?);
        }
    }
    Some(fnv1a(&text))
}

/// Allocated sections, in listing order.
pub(super) fn parse_sections(listing: &str) -> Vec<Section> {
    section_headers(listing)
        .filter(|h| h.flags.contains("ALLOC"))
        .map(|h| Section {
            name: h.name.to_string(),
            vma: h.vma,
            size: h.size,
            offset: h.offset,
            code: h.flags.contains("CODE"),
            contents: h.flags.contains("CONTENTS"),
            tls: h.flags.contains("THREAD_LOCAL"),
            writable: !h.flags.contains("READONLY") && !h.flags.contains("CODE"),
        })
        .collect()
}

/// The start and size of the `PT_GNU_RELRO` segment in an `objdump -p`
/// listing, which the loader makes read-only once it has relocated it.
pub(super) fn relro_range(headers: &str) -> Option<(u64, u64)> {
    let lines: Vec<&str> = headers.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.trim_start().starts_with("RELRO"))?;
    let field = |line: &str, key: &str| {
        let mut words = line.split_whitespace().skip_while(|w| *w != key);
        let value = words.nth(1)?;
        u64::from_str_radix(value.strip_prefix("0x")?, 16).ok()
    };
    Some((
        field(lines[at], "vaddr")?,
        field(lines.get(at + 1)?, "memsz")?,
    ))
}

/// `sections` with those wholly inside the `relro` range no longer writable.
pub(super) fn seal(sections: Vec<Section>, relro: Option<(u64, u64)>) -> Vec<Section> {
    let Some((start, size)) = relro else {
        return sections;
    };
    sections
        .into_iter()
        .map(|s| Section {
            writable: s.writable && !(s.vma >= start && s.vma + s.size <= start + size),
            ..s
        })
        .collect()
}

/// The sections an address can fall in, ordered by address. Thread-local
/// ones overlap the rest and are reached by offset instead.
pub(super) fn addressable(sections: Vec<Section>) -> Vec<Section> {
    let mut sections: Vec<Section> = sections.into_iter().filter(|s| !s.tls).collect();
    sections.sort_by_key(|s| (s.vma, s.size));
    sections
}

/// Sized symbols from `nm -S`, ordered by address.
pub(super) fn parse_symbols(listing: &str) -> Vec<Symbol> {
    let mut symbols: Vec<Symbol> = listing
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let [addr, size, kind, name] = f[..] else {
                return None;
            };
            let size = u64::from_str_radix(size, 16).ok().filter(|&s| s > 0)?;
            Some(Symbol {
                addr: u64::from_str_radix(addr, 16).ok()?,
                size,
                name: name.to_string(),
                func: matches!(kind, "T" | "t" | "W" | "w"),
            })
        })
        .collect();
    symbols.sort_by_key(|s| s.addr);
    symbols
}

/// An address that names no data to hash.
#[derive(Debug, PartialEq)]
pub(super) enum Marker {
    /// A Rust data symbol `nm` lists without a size: a zero sized static,
    /// whose address code takes and whose content is nothing (`zst:NAME`).
    Opaque(String),
    /// `__stop_SEC`, the linker's one-past-the-end of `section`
    /// (`bound:NAME`). It cannot be dereferenced, and often sits at the start
    /// of whatever follows, which code reads through its own references.
    End { name: String, section: usize },
}

/// The markers of `listing`, keyed by address. A `__start_SEC` is not one:
/// it is a real element address, so it resolves as data. A `__stop_SEC`
/// with no section `SEC` ending at it is none either.
///
/// A sized symbol at the same address is resolved first and wins.
pub(super) fn parse_markers(listing: &str, sections: &[Section]) -> HashMap<u64, Marker> {
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
            let in_data = sections.iter().any(|s| !s.code && s.contains(addr));
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
    let first: HashMap<u64, String> = named.into_iter().rev().collect();
    first
        .into_iter()
        .filter_map(|(addr, name)| {
            let marker = match name.strip_prefix("bound:__stop_") {
                None => Marker::Opaque(name),
                Some(sec) => {
                    let ends_here = |s: &Section| s.name == sec && s.vma + s.size == addr;
                    let section = sections.iter().position(ends_here)?;
                    Marker::End { name, section }
                }
            };
            Some((addr, marker))
        })
        .collect()
}

/// What a dynamic relocation writes into its slot at load time.
#[derive(Clone)]
pub(super) enum Target {
    /// An address inside this image.
    Relative(u64),
    /// A named symbol, with its addend.
    Symbol(String),
}

pub(super) struct Reloc {
    pub(super) offset: u64,
    pub(super) target: Target,
}

/// Dynamic relocations from `objdump -R`, ordered by slot address.
pub(super) fn parse_relocs(listing: &str) -> Vec<Reloc> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codekey::fixture::{SECTIONS, loaded};

    #[test]
    fn the_relro_segment_is_read_from_the_program_headers() {
        let headers = "\
    LOAD off    0x0000000000000000 vaddr 0x0000000000000000 paddr 0x0000000000000000 align 2**12
         filesz 0x0000000000000578 memsz 0x0000000000000578 flags r--
   RELRO off    0x0000000000000730 vaddr 0x0000000000002730 paddr 0x0000000000002730 align 2**0
         filesz 0x0000000000000198 memsz 0x00000000000008d0 flags r--
";
        assert_eq!(relro_range(headers), Some((0x2730, 0x8d0)));
        assert_eq!(relro_range("    LOAD off 0x0 vaddr 0x0\n"), None);
    }

    #[test]
    fn a_rust_data_symbol_without_a_size_is_zero_sized() {
        let listing = "\
0000000000001040 t _ZN3foo4code17h1E
0000000000002008 r _RNvCs1_3foo5EMPTY.llvm.9
0000000000002010 r _IO_stdin_used
0000000000002018 0000000000000008 r _RNvCs1_3foo5SIZED
";
        assert_eq!(
            parse_markers(listing, &loaded(SECTIONS)),
            HashMap::from([(0x2008, Marker::Opaque("zst:_RNvCs1_3foo5EMPTY".to_string()))])
        );
    }

    #[test]
    fn a_stop_symbol_is_named_and_a_start_symbol_is_not() {
        let sections = loaded(
            "\
Idx Name          Size      VMA               LMA               File off  Algn
  0 linkme_X      00000008  0000000000002000  0000000000002000  00002000  2**3
                  CONTENTS, ALLOC, LOAD, DATA
  1 .between      00000008  0000000000002008  0000000000002008  00002008  2**3
                  CONTENTS, ALLOC, LOAD, DATA
  2 linkme_A      00000008  0000000000002010  0000000000002010  00002010  2**3
                  CONTENTS, ALLOC, LOAD, DATA
  3 .after        00000008  0000000000002018  0000000000002018  00002018  2**3
                  CONTENTS, ALLOC, LOAD, DATA
",
        );
        let listing = "\
0000000000002008 r __stop_linkme_X
0000000000002010 r __start_linkme_Y
0000000000002018 r __stop_linkme_Z
0000000000002018 r __stop_linkme_A
";
        let end = |name: &str, section| Marker::End {
            name: name.to_string(),
            section,
        };
        assert_eq!(
            parse_markers(listing, &sections),
            HashMap::from([
                (0x2008, end("bound:__stop_linkme_X", 0)),
                (0x2018, end("bound:__stop_linkme_A", 2)),
            ])
        );
    }

    #[test]
    fn the_thread_local_image_is_hashed_and_other_sections_are_not() {
        let listing = "\
Idx Name          Size      VMA               LMA               File off  Algn
  0 .tdata        00000004  0000000000003000  0000000000003000  00000010  2**2
                  CONTENTS, ALLOC, LOAD, DATA, THREAD_LOCAL
  1 .tbss         00000008  0000000000003004  0000000000003004  00000014  2**3
                  ALLOC, THREAD_LOCAL
  2 .rodata       00000004  0000000000002000  0000000000002000  00000020  2**2
                  CONTENTS, ALLOC, LOAD, READONLY, DATA
";
        let digest = |tdata: u8, rodata: u8| {
            let mut image = vec![0; 0x40];
            image[0x10] = tdata;
            image[0x20] = rodata;
            tls_digest(&parse_sections(listing), &image)
        };
        assert_ne!(digest(4, 0), digest(64, 0));
        assert_eq!(digest(4, 0), digest(4, 9));
    }
}
