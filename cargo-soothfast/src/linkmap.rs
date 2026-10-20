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

use crate::headrun::binary_digest;
use crate::invoke::{self, CommonArgs};
use crate::runcache;

/// Maps kept before the oldest are dropped.
const KEEP: usize = 64;

/// One input section's place in the binary.
#[derive(Clone, Debug, PartialEq)]
pub struct Extent {
    pub vma: u64,
    pub size: u64,
    /// Size of one object when the section is a pool of fixed-size constants
    /// (`.rodata.cstN`), else 0.
    pub entry: u64,
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
            let section = line.rsplit_once(":(")?.1.strip_suffix(')')?;
            Some(Extent {
                vma,
                size,
                entry: entry_size(section),
            })
        })
        .collect()
}

/// GNU ld lists `.section 0xADDR 0xSIZE file` under an output section, with
/// the numbers on the next line when the name is long.
fn parse_gnu(text: &str) -> Vec<Extent> {
    let hex = |s: &str| u64::from_str_radix(s.strip_prefix("0x")?, 16).ok();
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
                if let (Some(vma), Some(size)) = (hex(addr), hex(size)) {
                    let entry = entry_size(name);
                    extents.push(Extent { vma, size, entry });
                }
            }
            [addr, size, ..] if named => {
                named = false;
                if let (Some(vma), Some(size)) = (hex(addr), hex(size)) {
                    let entry = entry_size(section);
                    extents.push(Extent { vma, size, entry });
                }
            }
            _ => named = false,
        }
    }
    extents
}

fn encode(extents: &[Extent]) -> String {
    extents
        .iter()
        .map(|e| format!("{:x} {:x} {:x}\n", e.vma, e.size, e.entry))
        .collect()
}

fn decode(text: &str) -> Vec<Extent> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split(' ').map(|n| u64::from_str_radix(n, 16).ok());
            Some(Extent {
                vma: f.next()??,
                size: f.next()??,
                entry: f.next()??,
            })
        })
        .collect()
}

/// The extents for the bench binary `exe`, built in `dir` (HEAD's tree when
/// `None`). Empty, with a line saying why, when no map can be had for this
/// exact binary; references into unnamed data then hash as a whole section.
pub fn extents(
    cmd: &str,
    common: &CommonArgs,
    exe: &Path,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
) -> Vec<Extent> {
    match obtain(common, exe, dir, target_dir) {
        Ok(extents) => extents,
        Err(why) => {
            println!(
                "{cmd}: no link map for {} ({why}); unnamed constants hash as whole sections",
                exe.display()
            );
            Vec::new()
        }
    }
}

fn obtain(
    common: &CommonArgs,
    exe: &Path,
    dir: Option<&Path>,
    target_dir: Option<&Path>,
) -> Result<Vec<Extent>, String> {
    let digest = binary_digest(exe).ok_or("binary unreadable")?;
    let maps = runcache::state_dir("maps").ok_or("no .soothfast directory")?;
    let stored = maps.join(format!("{digest}.extents"));
    if let Ok(text) = std::fs::read_to_string(&stored)
        && let extents = decode(&text)
        && !extents.is_empty()
    {
        return Ok(extents);
    }
    // Kept, not removed: cargo answers a repeated map build as fresh without
    // relinking, so the map from the first link is the only one there is.
    let map = maps.join(format!("{digest}.map"));
    let built = invoke::build_link_map(common, dir, target_dir, &map);
    let text = std::fs::read_to_string(&map);
    let built = built.ok_or("the map build failed")?;
    if binary_digest(&built).as_deref() != Some(digest.as_str()) {
        return Err("the map build links different code".into());
    }
    let extents = parse(&text.map_err(|e| e.to_string())?).ok_or("unrecognized map format")?;
    if std::fs::write(&stored, encode(&extents)).is_ok() {
        runcache::prune_oldest(&maps, KEEP);
    }
    Ok(extents)
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
";

    fn at(vma: u64, size: u64) -> Extent {
        Extent {
            vma,
            size,
            entry: 0,
        }
    }

    fn pool(vma: u64, size: u64, entry: u64) -> Extent {
        Extent { vma, size, entry }
    }

    #[test]
    fn an_lld_map_yields_its_input_sections() {
        assert_eq!(
            parse(LLD).expect("parsed"),
            [
                at(0x2e0, 0x1c),
                pool(0xce40, 0xd8, 8),
                at(0xcf18, 7),
                at(0xcf64, 0x11),
                pool(0xcf90, 0x10, 8)
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
            [at(0xd850, 1), at(0xd851, 20), pool(0xd868, 16, 16)]
        );
    }

    #[test]
    fn a_gnu_ld_map_yields_its_input_sections_with_wrapped_names() {
        assert_eq!(
            parse(GNU).expect("parsed"),
            [
                at(0x2000, 4),
                at(0x2008, 0x10),
                at(0x2018, 0x18),
                pool(0x2030, 0x20, 16)
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
        let extents = vec![at(0x2e0, 0x1c), at(0xcf18, 7)];
        assert_eq!(decode(&encode(&extents)), extents);
    }
}
