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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use soothfast_registry::fnv1a;

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
    /// same code. The object is its path below the profile directory, with
    /// the archive member when it has one. The stored extents keep its hash.
    pub name: String,
}

fn named(path: &str, section: &str, root: &str) -> String {
    let path = PathBuf::from(
        path.trim()
            .split('/')
            .filter(|part| !is_rustc_temp(part))
            .collect::<Vec<_>>()
            .join("/"),
    );
    let below = path.strip_prefix(root).unwrap_or(&path);
    if below.as_os_str().is_empty() || section.is_empty() {
        return String::new();
    }
    format!("{}:{section}", below.display())
}

/// Thread-local output sections, whose addresses are a template that overlaps
/// the sections after it.
fn is_tls(output: &str) -> bool {
    matches!(output, ".tdata" | ".tbss")
}

/// `extents` with each name replaced by its hash, which names an input as well
/// as the text does and is what the stored extents keep.
fn hashed(extents: Vec<Extent>) -> Vec<Extent> {
    extents
        .into_iter()
        .map(|e| Extent {
            name: match e.name.is_empty() {
                true => e.name,
                false => format!("{:016x}", fnv1a(e.name.as_bytes())),
            },
            ..e
        })
        .collect()
}

/// The profile directory (`target/[triple/]release`) of a bench executable.
/// Objects are named below it, so the checkout and the target triple drop out.
fn root_of(exe: &Path) -> String {
    exe.parent()
        .and_then(Path::parent)
        .map(|dir| std::path::absolute(dir).unwrap_or(dir.to_path_buf()))
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// The directory rustc copies the libraries into to link: `rustc` and six
/// random characters. It names no object.
fn is_rustc_temp(part: &str) -> bool {
    part.strip_prefix("rustc").is_some_and(|random| {
        random.len() == 6 && random.bytes().all(|b| b.is_ascii_alphanumeric())
    })
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
/// map, their objects named below `root` (a path outside it stays whole).
/// Inputs outside memory are dropped. `None` for any other format.
pub fn parse(text: &str, root: &str) -> Option<Vec<Extent>> {
    let header = text.lines().next()?;
    let mut extents = if header.contains("VMA") && header.contains("LMA") {
        parse_columns(text, 16, 16, root)
    } else if header.contains("VMA") {
        parse_columns(text, 16, 10, root)
    } else if text.contains("Linker script and memory map") {
        parse_gnu(text, root)
    } else {
        return None;
    };
    extents.retain(|e| e.size > 0 && e.vma > 0);
    extents.sort_by_key(|e| e.vma);
    number_repeats(&mut extents);
    (!extents.is_empty()).then_some(extents)
}

/// Tell inputs that share a name apart by their order in the binary: a
/// distributed slice puts every element in a section of one name, in one
/// object. Inserting an element renumbers the ones after it, which costs a
/// miss.
fn number_repeats(extents: &mut [Extent]) {
    let mut count: HashMap<String, usize> = HashMap::new();
    for e in extents.iter().filter(|e| !e.name.is_empty()) {
        *count.entry(e.name.clone()).or_default() += 1;
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    for e in extents.iter_mut().filter(|e| count.get(&e.name) > Some(&1)) {
        let n = seen.entry(e.name.clone()).or_default();
        e.name = format!("{}#{n}", e.name);
        *n += 1;
    }
}

/// lld writes `VMA LMA Size Align Out In Symbol` rows in bare hexadecimal;
/// mold writes `VMA Size Align Out In Symbol` with a `0x` VMA and a decimal
/// size. An input section is a row whose last column reads `file:(section)`.
/// An output section is a row with text in the `Out` column, and the inputs
/// of one at address 0 (`.debug_*`, `.comment`) are not in memory.
fn parse_columns(text: &str, vma_radix: u32, size_radix: u32, root: &str) -> Vec<Extent> {
    let lma = size_radix == 16;
    let columns = if lma { 4 } else { 3 };
    let out_column = text.lines().next().and_then(|h| h.find("Out"));
    let input = |line: &str| {
        let mut f = line.split_whitespace();
        let vma = f.next()?;
        let vma = u64::from_str_radix(vma.strip_prefix("0x").unwrap_or(vma), vma_radix).ok()?;
        if lma {
            f.next()?;
        }
        let size = u64::from_str_radix(f.next()?, size_radix).ok()?;
        let (path, section) = after_columns(line, columns).rsplit_once(":(")?;
        let section = section.strip_suffix(')')?;
        Some(Extent {
            vma,
            size,
            entry: entry_size(section),
            name: named(path, section, root),
        })
    };
    let mut extents = Vec::new();
    let mut in_memory = true;
    for line in text.lines() {
        if line.ends_with(')') && line.contains(":(") {
            extents.extend(input(line).filter(|_| in_memory));
        } else if out_column.is_some_and(|at| {
            line.as_bytes()
                .get(at)
                .is_some_and(|b| !b.is_ascii_whitespace())
        }) && let Some(vma) = line.split_whitespace().next()
        {
            let vma = vma.strip_prefix("0x").unwrap_or(vma);
            let output = after_columns(line, columns).split_whitespace().next();
            in_memory = u64::from_str_radix(vma, vma_radix) != Ok(0) && !output.is_some_and(is_tls);
        }
    }
    extents
}

/// GNU ld lists `.section 0xADDR 0xSIZE file` under an output section, with
/// the numbers on the next line when the name is long. An output section is
/// an unindented row of a name, an address and a size.
fn parse_gnu(text: &str, root: &str) -> Vec<Extent> {
    let hex = |s: &str| u64::from_str_radix(s.strip_prefix("0x")?, 16).ok();
    let extent = |addr: &str, size: &str, section: &str, path: &str| {
        Some(Extent {
            vma: hex(addr)?,
            size: hex(size)?,
            entry: entry_size(section),
            name: named(path, section, root),
        })
    };
    let mut extents = Vec::new();
    let mut named = false;
    let mut section = "";
    let mut in_memory = true;
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
                extents
                    .extend(extent(addr, size, name, after_columns(line, 3)).filter(|_| in_memory));
            }
            [addr, size, ..] if named => {
                named = false;
                extents.extend(
                    extent(addr, size, section, after_columns(line, 2)).filter(|_| in_memory),
                );
            }
            [name, addr, size, ..] if !nested && hex(addr).is_some() && hex(size).is_some() => {
                named = false;
                in_memory = hex(addr) != Some(0) && !is_tls(name);
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
                "{cmd}: no link map for {} ({why}); unnamed constants hash as whole sections, and code that reads a duplicate-named local static or writable data with no symbol or label is left unhashed",
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
    // Cargo answers a repeated build as fresh without relinking, and writes no
    // map then. A path it has not seen forces the link.
    let kept = maps.join(format!("{digest}.map"));
    let forced = maps.join(format!("{digest}.{}.map", std::process::id()));
    let mut linked = None;
    for map in [kept.clone(), forced.clone()] {
        let Some(built) = invoke::build_link_map(common, bin.dir, bin.target_dir, &map) else {
            break;
        };
        if let Ok(text) = std::fs::read_to_string(&map) {
            linked = Some((built, text));
            break;
        }
    }
    let _ = std::fs::remove_file(&kept);
    let _ = std::fs::remove_file(&forced);
    let (built, text) = linked.ok_or("the map build failed")?;
    if binary_digest(&built).as_deref() != Some(digest) {
        return Err("the map build links different code".into());
    }
    let extents = hashed(parse(&text, &root_of(&built)).ok_or("unrecognized map format")?);
    let encoded = encode(&extents);
    if std::fs::write(maps.join(format!("{digest}.extents")), &encoded).is_ok() {
        runcache::prune_oldest(&maps, KEEP);
    }
    if let Some(entry) = &entry {
        binstore::replace_sidecar(entry, "extents", &encoded);
    }
    Ok(extents)
}

/// The extents already worked out for the binary with `digest`, from `maps`
/// or from the binary store's `entry` for it. One found only in the entry is put back in `maps`.
fn cached(maps: &Path, digest: &str, entry: Option<&Path>) -> Option<Vec<Extent>> {
    let stored = maps.join(format!("{digest}.extents"));
    let local = std::fs::read_to_string(&stored)
        .ok()
        .map(|text| (text, false));
    let sidecar = || {
        let text = binstore::read_sidecar(entry?, "extents")?;
        Some((text, true))
    };
    let (extents, text, restored) =
        local
            .into_iter()
            .chain(sidecar())
            .find_map(|(text, restored)| {
                let extents = decode(&text);
                (!extents.is_empty()).then_some((extents, text, restored))
            })?;
    if restored && std::fs::write(&stored, &text).is_ok() {
        runcache::prune_oldest(maps, KEEP);
    }
    if let Some(entry) = entry
        && !restored
        && binstore::read_sidecar(entry, "extents").as_deref() != Some(text.as_str())
    {
        binstore::replace_sidecar(entry, "extents", &text);
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
            parse(LLD, "").expect("parsed"),
            [
                at(0x2e0, 0x1c, "<internal>:.interp"),
                pool(0xce40, 0xd8, 8, "<internal>:.rodata.cst8#0"),
                at(0xcf18, 7, "/w/target/deps/bench-1.o:.rodata..Lanon.1.0"),
                at(0xcf64, 0x11, "/w path/deps/bench-1.o:.rodata..Lanon.1.1"),
                pool(0xcf90, 0x10, 8, "<internal>:.rodata.cst8#1")
            ]
        );
    }

    #[test]
    fn inputs_outside_memory_are_dropped_and_dotless_outputs_are_outputs() {
        let map = [
            "             VMA              LMA     Size Align Out     In      Symbol",
            "            1000             1000       10     1 .data",
            "            1000             1000        8     1         /w/t/release/deps/a.o:(.data.y)",
            "            1000             1000        0     1                 .Lsym",
            "               0                0       40     1 .debug_info",
            "              10               10       40     1         /w/t/release/deps/a.o:(.debug_info)",
            "               0                0        8     1 .comment",
            "              10               10        8     1         <internal>:(.comment)",
            "            2000             2000       10     1 linkme_x",
            "            2000             2000        8     1         /w/t/release/deps/a.o:(linkme_x)",
        ]
        .join("\n");
        let names: Vec<String> = parse(&map, "/w/t/release")
            .expect("parsed")
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["deps/a.o:.data.y", "deps/a.o:linkme_x"]);
    }

    #[test]
    fn a_gnu_map_drops_inputs_of_outputs_at_address_zero() {
        let map = "\
Linker script and memory map

.data           0x0000000000003000        0x8
 .data.y        0x0000000000003000        0x8 a.o
.debug_info     0x0000000000000000       0x40
 .debug_info    0x0000000000000010       0x40 a.o
";
        let names: Vec<String> = parse(map, "")
            .expect("parsed")
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["a.o:.data.y"]);
    }

    #[test]
    fn a_hashed_name_keeps_inputs_apart_in_sixteen_digits() {
        let named = |name: &str| Extent {
            name: name.to_string(),
            ..Extent::default()
        };
        let out = hashed(vec![
            named("a.o:.bss.x"),
            named("b.o:.bss.x"),
            named("a.o:.bss.x"),
        ]);
        assert_eq!(out[0].name, out[2].name);
        assert_ne!(out[0].name, out[1].name);
        assert_eq!(out[0].name.len(), 16);
        assert_eq!(hashed(vec![named("")])[0].name, "");
    }

    #[test]
    fn inputs_of_one_name_are_numbered_in_address_order() {
        let map = [
            "             VMA              LMA     Size Align Out     In      Symbol",
            "            1000             1000       10     1 linkme_X",
            "            1000             1000        8     1         /w/t/release/deps/a.o:(linkme_X)",
            "            1008             1008        8     1         /w/t/release/deps/a.o:(linkme_X)",
            "            2000             2000        8     1         /w/t/release/deps/a.o:(.data.y)",
        ]
        .join("\n");
        let names: Vec<String> = parse(&map, "/w/t/release")
            .expect("parsed")
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(
            names,
            [
                "deps/a.o:linkme_X#0",
                "deps/a.o:linkme_X#1",
                "deps/a.o:.data.y"
            ]
        );
    }

    #[test]
    fn a_degenerate_name_is_no_name() {
        assert_eq!(named("", ".bss.x", ""), "");
        assert_eq!(named("/w/t", ".bss.x", "/w/t"), "");
        assert_eq!(named("/w/t/a.o", "", "/w/t"), "");
        assert_eq!(named("/w/t/a.o", ".bss.x", "/w/t"), "a.o:.bss.x");
    }

    #[test]
    fn thread_local_outputs_are_not_in_the_extents() {
        let map = [
            "             VMA              LMA     Size Align Out     In      Symbol",
            "            1000             1000       10     1 .tbss",
            "            1000             1000        8     1         /w/t/release/deps/a.o:(.tbss.x)",
            "            1000             1000       10     1 .init_array",
            "            1000             1000        8     1         /w/t/release/deps/a.o:(.init_array)",
        ]
        .join("\n");
        let names: Vec<String> = parse(&map, "/w/t/release")
            .expect("parsed")
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["deps/a.o:.init_array"]);
    }

    #[test]
    fn a_stored_executable_does_not_move_the_root() {
        let built = Path::new("/w/target/release/deps/bench-1");
        let stored = Path::new("/w/.soothfast/bins/key-digest.bin");
        assert_eq!(root_of(built), "/w/target/release");
        assert_ne!(root_of(stored), root_of(built));
    }

    #[test]
    fn objects_are_named_below_the_target_directory() {
        let map = LLD.replace(".rodata", ".data");
        let names: Vec<String> = parse(&map, "/w/target")
            .expect("parsed")
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(names.contains(&"deps/bench-1.o:.data..Lanon.1.0".to_string()));
        assert!(names.contains(&"/w path/deps/bench-1.o:.data..Lanon.1.1".to_string()));
    }

    #[test]
    fn the_linkers_temporary_directory_is_not_part_of_an_objects_name() {
        let map = |temp: &str| {
            format!(
                "             VMA              LMA     Size Align Out     In      Symbol\n            1000             1000        4     1         /w/t/release/deps/{temp}/libfoo.rlib(bar.o):(.bss.x)\n"
            )
        };
        let name = |temp: &str| parse(&map(temp), "/w/t").expect("parsed")[0].name.clone();
        assert_eq!(name("rustcAbC123"), name("rustcXyZ789"));
        assert_eq!(
            name("rustcAbC123"),
            "release/deps/libfoo.rlib(bar.o):.bss.x"
        );
    }

    #[test]
    fn a_root_matches_whole_directories_only() {
        let map = "\
             VMA              LMA     Size Align Out     In      Symbol
            1000             1000        4     1         /w/tmp/util.o:(.bss.x)
            1004             1004        4     1         /w/t/util.o:(.bss.x)
";
        let names: Vec<String> = parse(map, "/w/t")
            .expect("parsed")
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["/w/tmp/util.o:.bss.x", "util.o:.bss.x"]);
    }

    #[test]
    fn a_triple_layout_and_a_worktree_layout_name_an_input_alike() {
        let map = |object: &str| {
            format!(
                "             VMA              LMA     Size Align Out     In      Symbol\n            1000             1000        4     1         {object}:(.bss.x)\n"
            )
        };
        let name = |exe: &str, object: &str| {
            parse(&map(object), &root_of(Path::new(exe))).expect("parsed")[0]
                .name
                .clone()
        };
        let head = name(
            "/w/target/x86_64-unknown-linux-gnu/release/deps/bench-1",
            "/w/target/x86_64-unknown-linux-gnu/release/deps/libfoo.rlib(bar.o)",
        );
        let base = name(
            "/tmp/wt/target/x86_64-unknown-linux-gnu/release/deps/bench-1",
            "/tmp/wt/target/x86_64-unknown-linux-gnu/release/deps/libfoo.rlib(bar.o)",
        );
        let host = name(
            "/tmp/wt/target/release/deps/bench-1",
            "/tmp/wt/target/release/deps/libfoo.rlib(bar.o)",
        );
        assert_eq!(head, "deps/libfoo.rlib(bar.o):.bss.x");
        assert_eq!(head, base);
        assert_eq!(head, host);
    }

    #[test]
    fn two_objects_of_one_file_name_in_different_directories_are_told_apart() {
        let map = "\
             VMA              LMA     Size Align Out     In      Symbol
            1000             1000        8     1 .bss
            1000             1000        4     1         /w/t/release/build/a/out/util.o:(.bss.x)
            1004             1004        4     1         /w/t/release/build/b/out/util.o:(.bss.x)
            1008             1008        4     1         /w/t/release/deps/libfoo.rlib(bar.o):(.bss.x)
";
        let names: Vec<String> = parse(map, "/w/t")
            .expect("parsed")
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(
            names,
            [
                "release/build/a/out/util.o:.bss.x",
                "release/build/b/out/util.o:.bss.x",
                "release/deps/libfoo.rlib(bar.o):.bss.x",
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
            parse(MOLD, "").expect("parsed"),
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
            parse(GNU, "").expect("parsed"),
            [
                at(0x2000, 4, "/usr/lib/crt1.o:.rodata"),
                at(0x2008, 0x10, "foo.o:.rodata.foo"),
                at(0x2018, 0x18, "bar.o:.rodata._ZN3bar17h0123456789abcdefE"),
                pool(0x2030, 0x20, 16, "baz.o:.rodata.cst16"),
                at(0x4000, 8, "/w path/deps/a b.o:.bss.counter"),
                at(
                    0x4008,
                    8,
                    "/w path/deps/baz.o:.bss._ZN3baz17h0123456789abcdefE"
                )
            ]
        );
    }

    #[test]
    fn an_unknown_map_format_yields_nothing() {
        assert_eq!(parse("not a map\n", ""), None);
        assert_eq!(parse("", ""), None);
    }

    #[test]
    fn extents_round_trip_through_their_stored_form() {
        let extents = vec![at(0x2e0, 0x1c, "a.o:.data"), at(0xcf18, 7, "b.o:.bss.x")];
        assert_eq!(decode(&encode(&extents)), extents);
    }

    #[test]
    fn an_old_extents_file_falls_through_to_the_stored_binary() {
        let root = std::env::temp_dir().join(format!("soothfast-sidecar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (maps, bins) = (root.join("maps"), root.join("bins"));
        std::fs::create_dir_all(&maps).unwrap();
        std::fs::create_dir_all(&bins).unwrap();
        let entry = bins.join("k-d.bin");
        std::fs::write(&entry, "binary").unwrap();
        std::fs::write(maps.join("abc.extents"), "2e0 1c 0\n").unwrap();
        let extents = vec![at(0x2e0, 0x1c, "a.o:.data")];
        binstore::write_sidecar(&entry, "extents", &encode(&extents));
        assert_eq!(cached(&maps, "abc", Some(&entry)), Some(extents.clone()));
        assert_eq!(
            decode(&std::fs::read_to_string(maps.join("abc.extents")).unwrap()),
            extents
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stored_form_without_names_is_a_cache_miss() {
        assert_eq!(decode("2e0 1c 0\n"), []);
        let root = std::env::temp_dir().join(format!("soothfast-oldmap-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("abc.extents"), "2e0 1c 0\n").unwrap();
        assert_eq!(cached(&root, "abc", None), None);
        let _ = std::fs::remove_dir_all(&root);
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
