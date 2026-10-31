//! Input-section extents of a bench binary, read from the linker's map.
//!
//! rustc gives every constant its own input section, and code cannot read
//! past the section it addresses, so an input section is a real object
//! boundary even where the symbol table has no symbol for it. `codekey`
//! hashes a reference into unnamed data by the extent found here, and by the
//! whole output section where there is none.
//!
//! The map comes from a second build of the bench target alone
//! (`cargo rustc`), which links a separate artifact of the same bytes and
//! leaves dependencies and the measured binary untouched. The map and the
//! extents read from it are kept under `.soothfast/maps/`, named by the
//! digest of the binary they describe, so they are never read for another.

use std::path::Path;

use crate::binstore;
use crate::headrun::binary_digest;
use crate::invoke::{self, CommonArgs};
use crate::itemreuse::Bin;
use crate::runcache;

/// Maps kept before the oldest are dropped.
pub(crate) const KEEP: usize = 64;

/// One input section's place in the binary.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extent {
    pub vma: u64,
    pub size: u64,
    /// Size of one object when the section is a pool of fixed-size constants
    /// (`.rodata.cstN`), else 0.
    pub entry: u64,
    /// `object:section`, which names the same input in another link of the
    /// same code.
    pub name: String,
}

/// `object:section`, the object being the last component of its path.
fn named(path: &str, section: &str) -> String {
    let object = path.trim().rsplit('/').next().unwrap_or_default();
    format!("{object}:{section}")
}

/// `line` after its first `columns` whitespace-separated fields.
fn after_columns(line: &str, columns: usize) -> &str {
    (0..columns).fold(line.trim_start(), |rest, _| {
        rest.split_once(char::is_whitespace)
            .map_or("", |(_, tail)| tail.trim_start())
    })
}

/// `.rodata.cst8` holds 8-byte objects; other names are one object.
fn entry_size(section: &str) -> u64 {
    section
        .strip_prefix(".rodata.cst")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// The extents of the input sections in `text`, read as an lld, mold or GNU ld
/// map. `None` for any other format.
pub fn parse(text: &str) -> Option<Vec<Extent>> {
    let header = text.lines().next()?;
    let mut extents = if header.contains("VMA") && header.contains("LMA") {
        parse_columns(text, 16, 16)
    } else if header.contains("VMA") {
        parse_columns(text, 16, 10)
    } else if text.contains("Linker script and memory map") {
        parse_gnu(text)
    } else {
        return None;
    };
    extents.retain(|e| e.size > 0);
    extents.sort_by_key(|e| e.vma);
    (!extents.is_empty()).then_some(extents)
}

/// lld writes `VMA LMA Size Align Out In Symbol` rows in bare hexadecimal;
/// mold writes `VMA Size Align Out In Symbol` with a `0x` VMA and a decimal
/// size. An input section is a row whose last column reads `file:(section)`.
fn parse_columns(text: &str, vma_radix: u32, size_radix: u32) -> Vec<Extent> {
    let lma = size_radix == 16;
    text.lines()
        .filter(|line| line.ends_with(')') && line.contains(":("))
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let vma = f.next()?;
            let vma = u64::from_str_radix(vma.strip_prefix("0x").unwrap_or(vma), vma_radix).ok()?;
            if lma {
                f.next()?;
            }
            let size = u64::from_str_radix(f.next()?, size_radix).ok()?;
            let (path, section) = after_columns(line, if lma { 4 } else { 3 }).rsplit_once(":(")?;
            let section = section.strip_suffix(')')?;
            Some(Extent {
                vma,
                size,
                entry: entry_size(section),
                name: named(path, section),
            })
        })
        .collect()
}

/// GNU ld lists `.section 0xADDR 0xSIZE file` under an output section, with
/// the numbers on the next line when the name is long.
fn parse_gnu(text: &str) -> Vec<Extent> {
    let hex = |s: &str| u64::from_str_radix(s.strip_prefix("0x")?, 16).ok();
    let extent = |addr: &str, size: &str, section: &str, path: &str| {
        Some(Extent {
            vma: hex(addr)?,
            size: hex(size)?,
            entry: entry_size(section),
            name: named(path, section),
        })
    };
    let mut extents = Vec::new();
    let mut named = false;
    let mut section = "";
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let nested = line.starts_with(' ');
        match f[..] {
            [name] if nested && name.starts_with('.') => {
                named = true;
                section = name;
            }
            [name, addr, size, ..] if nested && name.starts_with('.') => {
                named = false;
                extents.extend(extent(addr, size, name, after_columns(line, 3)));
            }
            [addr, size, ..] if named => {
                named = false;
                extents.extend(extent(addr, size, section, after_columns(line, 2)));
            }
            _ => named = false,
        }
    }
    extents
}

fn encode(extents: &[Extent]) -> String {
    extents
        .iter()
        .map(|e| format!("{:x} {:x} {:x} {}\n", e.vma, e.size, e.entry, e.name))
        .collect()
}

/// Empty for a stored form without names, which is read as no map at all.
fn decode(text: &str) -> Vec<Extent> {
    text.lines()
        .map(|line| {
            let mut f = line.splitn(4, ' ');
            let mut number = || u64::from_str_radix(f.next()?, 16).ok();
            Some(Extent {
                vma: number()?,
                size: number()?,
                entry: number()?,
                name: f.next()?.to_string(),
            })
        })
        .collect::<Option<_>>()
        .unwrap_or_default()
}

/// The extents for the bench binary `bin`, built in its `dir` (HEAD's tree
/// when `None`). Empty, with a line saying why, when no map can be had for this
/// exact binary; references into unnamed data then hash as a whole section.
pub fn extents(cmd: &str, common: &CommonArgs, bin: &Bin) -> Vec<Extent> {
    match obtain(common, bin) {
        Ok(extents) => extents,
        Err(why) => {
            println!(
                "{cmd}: no link map for {} ({why}); unnamed constants hash as whole sections",
                bin.exe.display()
            );
            Vec::new()
        }
    }
}

fn obtain(common: &CommonArgs, bin: &Bin) -> Result<Vec<Extent>, String> {
    let digest = bin.digest.ok_or("binary unreadable")?;
    let maps = runcache::state_dir("maps").ok_or("no .soothfast directory")?;
    let entry = binstore::entry_of(bin.exe);
    if let Some(extents) = cached(&maps, digest, entry.as_deref()) {
        return Ok(extents);
    }
    // Kept, not removed: cargo answers a repeated map build as fresh without
    // relinking, so the map from the first link is the only one there is.
    let map = maps.join(format!("{digest}.map"));
    let built = invoke::build_link_map(common, bin.dir, bin.target_dir, &map);
    let text = std::fs::read_to_string(&map);
    let built = built.ok_or("the map build failed")?;
    if binary_digest(&built).as_deref() != Some(digest) {
        return Err("the map build links different code".into());
    }
    let extents = parse(&text.map_err(|e| e.to_string())?).ok_or("unrecognized map format")?;
    let encoded = encode(&extents);
    if std::fs::write(maps.join(format!("{digest}.extents")), &encoded).is_ok() {
        runcache::prune_oldest(&maps, KEEP);
    }
    if let Some(entry) = &entry {
        binstore::write_sidecar(entry, "extents", &encoded);
    }
    Ok(extents)
}

/// The extents already worked out for the binary with `digest`, from `maps`
/// or from the binary store's `entry` for it. One found only in the entry is put back in `maps`.
fn cached(maps: &Path, digest: &str, entry: Option<&Path>) -> Option<Vec<Extent>> {
    let stored = maps.join(format!("{digest}.extents"));
    let text = match std::fs::read_to_string(&stored) {
        Ok(text) => text,
        Err(_) => {
            let text = binstore::read_sidecar(entry?, "extents")?;
            if std::fs::write(&stored, &text).is_ok() {
                runcache::prune_oldest(maps, KEEP);
            }
            text
        }
    };
    let extents = decode(&text);
    if extents.is_empty() {
        return None;
    }
    if let Some(entry) = entry {
        binstore::write_sidecar(entry, "extents", &text);
    }
    Some(extents)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LLD: &str = "\
             VMA              LMA     Size Align Out     In      Symbol
             2e0              2e0       1c     1 .interp
             2e0              2e0       1c     1         <internal>:(.interp)
            ce40             ce40     93d8    16 .rodata
            ce40             ce40       d8     8         <internal>:(.rodata.cst8)
            cf18             cf18        7     1         /w/target/deps/bench-1.o:(.rodata..Lanon.1.0)
            cf50             cf50        0     1         /w/target/deps/bench-1.o:(.rodata.EMPTY)
            cf50             cf50        0     1                 soothfast::EMPTY
            cf64             cf64       11     1         /w path/deps/bench-1.o:(.rodata..Lanon.1.1)
            cf80             cf80        0     1                 std::thread::Thread::new(x:(y))
            cf90             cf90       10     8         <internal>:(.rodata.cst8)
";

    const GNU: &str = "\
Memory Configuration

Linker script and memory map

.rodata         0x0000000000002000       0x30
 *(.rodata .rodata.* .gnu.linkonce.r.*)
 .rodata        0x0000000000002000        0x4 /usr/lib/crt1.o
                0x0000000000002000                _IO_stdin_used
 .rodata.foo    0x0000000000002008       0x10 foo.o
 .rodata._ZN3bar17h0123456789abcdefE
                0x0000000000002018       0x18 bar.o
 .rodata.cst16
                0x0000000000002030       0x20 baz.o
.data           0x0000000000003000        0x8
.bss
 .bss.counter   0x0000000000004000        0x8 /w path/deps/a b.o
 .bss._ZN3baz17h0123456789abcdefE
                0x0000000000004008        0x8 /w path/deps/baz.o
";

    fn at(vma: u64, size: u64, name: &str) -> Extent {
        Extent {
            vma,
            size,
            entry: 0,
            name: name.to_string(),
        }
    }

    fn pool(vma: u64, size: u64, entry: u64, name: &str) -> Extent {
        Extent {
            entry,
            ..at(vma, size, name)
        }
    }

    #[test]
    fn an_lld_map_yields_its_input_sections() {
        assert_eq!(
            parse(LLD).expect("parsed"),
            [
                at(0x2e0, 0x1c, "<internal>:.interp"),
                pool(0xce40, 0xd8, 8, "<internal>:.rodata.cst8"),
                at(0xcf18, 7, "bench-1.o:.rodata..Lanon.1.0"),
                at(0xcf64, 0x11, "bench-1.o:.rodata..Lanon.1.1"),
                pool(0xcf90, 0x10, 8, "<internal>:.rodata.cst8")
            ]
        );
    }

    const MOLD: &str = "\
               VMA       Size Align Out     In      Symbol
                 0         64     8 EHDR
            0xd850      16120     8 .rodata
            0xd850          1     1         m.cgu.0.rcgu.o:(.rodata..Lanon.1)
            0xd851         20     1         m.cgu.0.rcgu.o:(.rodata..Lanon.2)
            0xd868         16    16         <internal>:(.rodata.cst16)
";

    #[test]
    fn a_mold_map_reads_its_hex_addresses_and_decimal_sizes() {
        assert_eq!(
            parse(MOLD).expect("parsed"),
            [
                at(0xd850, 1, "m.cgu.0.rcgu.o:.rodata..Lanon.1"),
                at(0xd851, 20, "m.cgu.0.rcgu.o:.rodata..Lanon.2"),
                pool(0xd868, 16, 16, "<internal>:.rodata.cst16")
            ]
        );
    }

    #[test]
    fn a_gnu_ld_map_yields_its_input_sections_with_wrapped_names() {
        assert_eq!(
            parse(GNU).expect("parsed"),
            [
                at(0x2000, 4, "crt1.o:.rodata"),
                at(0x2008, 0x10, "foo.o:.rodata.foo"),
                at(0x2018, 0x18, "bar.o:.rodata._ZN3bar17h0123456789abcdefE"),
                pool(0x2030, 0x20, 16, "baz.o:.rodata.cst16"),
                at(0x4000, 8, "a b.o:.bss.counter"),
                at(0x4008, 8, "baz.o:.bss._ZN3baz17h0123456789abcdefE")
            ]
        );
    }

    #[test]
    fn an_unknown_map_format_yields_nothing() {
        assert_eq!(parse("not a map\n"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn extents_round_trip_through_their_stored_form() {
        let extents = vec![at(0x2e0, 0x1c, "a.o:.data"), at(0xcf18, 7, "b.o:.bss.x")];
        assert_eq!(decode(&encode(&extents)), extents);
    }

    #[test]
    fn a_stored_form_without_names_is_no_map() {
        assert_eq!(decode("2e0 1c 0\n"), []);
    }

    #[test]
    fn a_stored_binary_restores_its_extents_into_an_empty_maps_directory() {
        let root = std::env::temp_dir().join(format!("soothfast-linkmap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (maps, bins) = (root.join("maps"), root.join("bins"));
        std::fs::create_dir_all(&maps).unwrap();
        std::fs::create_dir_all(&bins).unwrap();
        let entry = bins.join("k-d.bin");
        std::fs::write(&entry, "binary").unwrap();
        assert_eq!(cached(&maps, "abc", Some(&entry)), None);

        let extents = vec![at(0x2e0, 0x1c, "a.o:.data"), at(0xcf18, 7, "b.o:.bss.x")];
        binstore::write_sidecar(&entry, "extents", &encode(&extents));
        assert_eq!(cached(&maps, "abc", Some(&entry)), Some(extents.clone()));
        assert!(maps.join("abc.extents").exists());
        assert_eq!(cached(&maps, "abc", None), Some(extents));
        let _ = std::fs::remove_dir_all(&root);
    }
}
