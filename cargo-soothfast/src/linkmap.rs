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
//! leaves dependencies and the measured binary untouched. It is kept as a
//! list of extents under `.soothfast/maps/`, named by the digest of the
//! binary it describes, so it is never read for another binary.

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
}

/// The extents of the input sections in `text`, read as an lld or mold map
/// (the same layout) or a GNU ld map. `None` for any other format.
pub fn parse(text: &str) -> Option<Vec<Extent>> {
    let mut extents = if text.lines().next()?.contains("VMA") {
        parse_lld(text)
    } else if text.contains("Linker script and memory map") {
        parse_gnu(text)
    } else {
        return None;
    };
    extents.retain(|e| e.size > 0);
    extents.sort_by_key(|e| e.vma);
    (!extents.is_empty()).then_some(extents)
}

/// `VMA LMA Size Align Out In Symbol` rows, hexadecimal without a prefix. An
/// input section is a row whose last column reads `file:(section)`.
fn parse_lld(text: &str) -> Vec<Extent> {
    text.lines()
        .filter(|line| line.ends_with(')') && line.contains(":("))
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let vma = u64::from_str_radix(f.next()?, 16).ok()?;
            f.next()?;
            let size = u64::from_str_radix(f.next()?, 16).ok()?;
            Some(Extent { vma, size })
        })
        .collect()
}

/// GNU ld lists `.section 0xADDR 0xSIZE file` under an output section, with
/// the numbers on the next line when the name is long.
fn parse_gnu(text: &str) -> Vec<Extent> {
    let hex = |s: &str| u64::from_str_radix(s.strip_prefix("0x")?, 16).ok();
    let mut extents = Vec::new();
    let mut named = false;
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let nested = line.starts_with(' ');
        match f[..] {
            [name] if nested && name.starts_with('.') => named = true,
            [name, addr, size, ..] if nested && name.starts_with('.') => {
                named = false;
                if let (Some(vma), Some(size)) = (hex(addr), hex(size)) {
                    extents.push(Extent { vma, size });
                }
            }
            [addr, size, ..] if named => {
                named = false;
                if let (Some(vma), Some(size)) = (hex(addr), hex(size)) {
                    extents.push(Extent { vma, size });
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
        .map(|e| format!("{:x} {:x}\n", e.vma, e.size))
        .collect()
}

fn decode(text: &str) -> Vec<Extent> {
    text.lines()
        .filter_map(|line| {
            let (vma, size) = line.split_once(' ')?;
            Some(Extent {
                vma: u64::from_str_radix(vma, 16).ok()?,
                size: u64::from_str_radix(size, 16).ok()?,
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
    if let Ok(text) = std::fs::read_to_string(&stored) {
        return Ok(decode(&text));
    }
    let map = maps.join(format!("{digest}.map"));
    let built = invoke::build_link_map(common, dir, target_dir, &map);
    let text = std::fs::read_to_string(&map);
    let _ = std::fs::remove_file(&map);
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
.data           0x0000000000003000        0x8
";

    fn at(vma: u64, size: u64) -> Extent {
        Extent { vma, size }
    }

    #[test]
    fn an_lld_or_mold_map_yields_its_input_sections() {
        assert_eq!(
            parse(LLD).expect("parsed"),
            [
                at(0x2e0, 0x1c),
                at(0xce40, 0xd8),
                at(0xcf18, 7),
                at(0xcf64, 0x11)
            ]
        );
    }

    #[test]
    fn a_gnu_ld_map_yields_its_input_sections_with_wrapped_names() {
        assert_eq!(
            parse(GNU).expect("parsed"),
            [at(0x2000, 4), at(0x2008, 0x10), at(0x2018, 0x18)]
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
