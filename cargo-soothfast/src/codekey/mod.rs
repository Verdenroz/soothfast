//! A hash of the machine code an item executed, read from a bench binary.
//!
//! The functions come from a callgrind run (see `soothfast_measure::callgrind`),
//! so two binaries that hash a function set alike ran the same instructions
//! over the same constants. Addresses are normalized out: branch targets and
//! rip-relative displacements become symbolic, and a data reference becomes
//! the content of the symbol (or, with no sized symbol, the section) it points
//! into. Anything that cannot be resolved leaves the function unhashed, which
//! the caller sees as a miss. aarch64 `adrp`+`add` immediates carry layout and
//! only ever cause misses.

mod disasm;
mod elf;
mod exits;
#[cfg(test)]
mod fixture;
mod identity;
mod resolve;
mod tables;

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io::BufReader;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, Once, OnceLock};

use soothfast_registry::fnv1a;

use crate::linkmap::Extent;
use disasm::hash_functions;
pub(crate) use elf::section_headers;
use elf::{
    addressable, parse_labels, parse_markers, parse_relocs, parse_sections, parse_symbols,
    relro_range, seal, tls_digest,
};
use resolve::Resolver;

/// Stands for the bench binary in a stored function set, so two builds at
/// different paths name the same functions.
pub const EXE: &str = "@exe";

/// The `.llvm.N` segment is per-build; the rest of the name, an addend
/// included, is the identity.
fn strip_llvm(name: &str) -> Cow<'_, str> {
    let Some(at) = name.find(".llvm.") else {
        return Cow::Borrowed(name);
    };
    let rest = &name[at + ".llvm.".len()..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    Cow::Owned(format!("{}{}", &name[..at], &rest[digits..]))
}

/// A piece of another function rather than a function: control jumps into it
/// and back. `.cold` is a split-off cold path (gcc, LLVM); `.__part.` and
/// `.lto_priv` are renamed pieces. gcc's `.part.N` is not one: it is a
/// callable function holding the tail of its parent, which returns to the
/// parent's caller, so a jump to it is a tail call like any other.
fn is_fragment(name: &str) -> bool {
    [".cold", ".__part.", ".lto_priv"]
        .iter()
        .any(|piece| name.contains(piece))
}

/// A Rust symbol: v0 (`_R...`) or legacy (`_ZN...17h<16 hex>E`).
fn is_rust_symbol(name: &str) -> bool {
    let name = strip_llvm(name.strip_prefix(LABELED).unwrap_or(name));
    if name.starts_with("_R") {
        return true;
    }
    let hash = name
        .strip_prefix("_ZN")
        .and_then(|rest| rest.strip_suffix('E'))
        .and_then(|rest| rest.rsplit_once("17h"))
        .map(|(_, hash)| hash);
    hash.is_some_and(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Callgrind's name for the frames under `main`, which is no symbol.
const BELOW_MAIN: &str = "(below main)";

/// Prefix of a portable name for code callgrind could only name by address
/// (a symbol without a size, a PLT stub): the label objdump gives it.
const LABELED: &str = "at:";

/// A symbol without callgrind's `'N` marker for functions inside a recursion
/// cycle.
fn unmarked(sym: &str) -> &str {
    match sym.rsplit_once('\'') {
        Some((name, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => sym,
    }
}

/// The bench-binary symbols among `functions`.
pub fn exe_symbols<'a>(
    functions: impl IntoIterator<Item = &'a (String, String)>,
) -> BTreeSet<String> {
    functions
        .into_iter()
        .filter(|(ob, _)| ob == EXE)
        .map(|(_, sym)| sym.clone())
        .collect()
}

/// `functions` as a stored entry keeps them: the object that is `bin` named
/// [`EXE`] with its symbols normalized, every other object reduced to its
/// path (its content decides what ran in it), sorted and deduplicated.
pub fn portable(bin: &Path, functions: &[(String, String)]) -> Vec<(String, String)> {
    let canonical = bin.canonicalize().ok();
    let is_bin = |ob: &str| Path::new(ob) == bin || canonical.as_deref() == Some(Path::new(ob));
    let set: BTreeSet<(String, String)> = functions
        .iter()
        .filter(|(ob, sym)| !(is_bin(ob) && sym == BELOW_MAIN))
        .map(|(ob, sym)| {
            if is_bin(ob) {
                (EXE.to_string(), strip_llvm(unmarked(sym)).to_string())
            } else {
                (ob.clone(), String::new())
            }
        })
        .collect();
    set.into_iter().collect()
}

fn command_text(program: &str, args: &[&str], bin: &Path) -> Option<String> {
    let out = Command::new(program).args(args).arg(bin).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The requested functions of one bench binary, hashed.
pub struct Binary {
    hashes: HashMap<String, u64>,
    /// Code the wanted set named by address, to its `at:` name.
    labels: HashMap<String, String>,
    /// Folded into every key: a thread-local's initial value is data no
    /// instruction names by address.
    tls: u64,
}

impl Binary {
    /// Hash `wanted` (portable names) in `bin`. `None` where `nm` or `objdump`
    /// is missing or fails; a function that cannot be named uniquely or
    /// resolved is simply absent.
    pub fn open(bin: &Path, wanted: &BTreeSet<String>, inputs: &[Extent]) -> Option<Binary> {
        let headers = command_text("objdump", &["-h"], bin)?;
        let sections = parse_sections(&headers);
        let listing = command_text("nm", &["-S", "--defined-only"], bin)?;
        let symbols = parse_symbols(&listing);
        // Fails on a binary with no dynamic section, which has no relocations.
        let relocs = parse_relocs(&command_text("objdump", &["-R"], bin).unwrap_or_default());
        let image = std::fs::read(bin).ok()?;
        // Only a position-independent image (ET_DYN) names every address it
        // uses rip-relative; a fixed-address one hides data in immediates.
        if image.get(..4) != Some(b"\x7fELF") || image.get(16..18) != Some(&[3, 0]) {
            return None;
        }
        let tls = tls_digest(&sections, &image)?;
        let relro = relro_range(&image);
        if relro.is_none() {
            static NOTED: Once = Once::new();
            NOTED.call_once(|| {
                eprintln!(
                    "codekey: no PT_GNU_RELRO segment in {}; relocated data counts as writable",
                    bin.display()
                );
            });
        }
        let sections = addressable(seal(sections, relro));
        let markers = parse_markers(&listing, &sections);
        let data_labels = parse_labels(&listing, &sections);
        let resolver = Resolver::new(
            &sections,
            &symbols,
            &relocs,
            &markers,
            &data_labels,
            inputs,
            &image,
        );
        let mut child = Command::new("objdump")
            .args(["-d", "--no-show-raw-insn"])
            .arg(bin)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let mut labels = HashMap::new();
        let hashes = hash_functions(&resolver, wanted, BufReader::new(stdout), &mut labels);
        child.wait().ok()?.success().then_some(Binary {
            hashes,
            labels,
            tls,
        })
    }

    #[cfg(test)]
    pub fn with_hashes(hashes: HashMap<String, u64>) -> Binary {
        Binary {
            hashes,
            labels: HashMap::new(),
            tls: 0,
        }
    }

    /// `functions` with each address replaced by its `at:` name; `None` if
    /// one has no label here.
    pub fn name_addresses(
        &self,
        functions: Vec<(String, String)>,
    ) -> Option<Vec<(String, String)>> {
        functions
            .into_iter()
            .map(|(ob, sym)| {
                if ob == EXE && sym.starts_with("0x") {
                    Some((ob, self.labels.get(&sym)?.clone()))
                } else {
                    Some((ob, sym))
                }
            })
            .collect()
    }

    /// The key of `functions` (as [`portable`] returns them) in this binary,
    /// or `None` if any is unhashed here or any foreign object is unreadable.
    pub fn code_key(&self, functions: &[(String, String)]) -> Option<String> {
        let mut text = format!("tls {:016x}\n", self.tls);
        for (ob, sym) in functions {
            if ob != EXE {
                writeln!(text, "f {ob} {:016x}", file_digest(ob)?).ok()?;
            } else {
                writeln!(text, "x {sym} {:016x}", self.hashes.get(sym)?).ok()?;
            }
        }
        Some(format!("{:016x}", fnv1a(text.as_bytes())))
    }

    /// Each bench-binary function of `functions` this binary has no hash for.
    pub fn unhashed(&self, functions: &[(String, String)]) -> Vec<String> {
        functions
            .iter()
            .filter(|(ob, sym)| ob == EXE && !self.hashes.contains_key(sym))
            .map(|(ob, sym)| format!("{ob} {sym}"))
            .collect()
    }

    /// Each bench-binary function of `functions` whose hash here differs
    /// from `other`'s, and the thread-local image when it does.
    pub fn differences(&self, other: &Binary, functions: &[(String, String)]) -> Vec<String> {
        let mut out = Vec::new();
        if self.tls != other.tls {
            out.push(format!(
                "thread-local image {:016x} vs {:016x}",
                self.tls, other.tls
            ));
        }
        for (ob, sym) in functions.iter().filter(|(ob, _)| ob == EXE) {
            let (a, b) = (self.hashes.get(sym), other.hashes.get(sym));
            if a != b {
                let show =
                    |h: Option<&u64>| h.map_or("unhashed".to_string(), |h| format!("{h:016x}"));
                out.push(format!("{ob} {sym}: {} vs {}", show(a), show(b)));
            }
        }
        out
    }
}

/// Content digest of a foreign object, read once per path. Callgrind's `???`
/// is valgrind's own trampolines, which the host key pins by valgrind version.
fn file_digest(path: &str) -> Option<u64> {
    if path == "???" {
        return Some(0);
    }
    static MEMO: OnceLock<Mutex<HashMap<String, Option<u64>>>> = OnceLock::new();
    let mut memo = MEMO.get_or_init(Default::default).lock().ok()?;
    if let Some(digest) = memo.get(path) {
        return *digest;
    }
    let digest = std::fs::read(path).ok().map(|b| fnv1a(&b));
    memo.insert(path.to_string(), digest);
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llvm_suffixes_drop_but_addends_stay() {
        assert_eq!(strip_llvm("foo.llvm.12+0x10"), "foo+0x10");
        assert_eq!(strip_llvm("foo.llvm.12"), "foo");
        assert_eq!(strip_llvm("foo+0x10"), "foo+0x10");
    }

    #[test]
    fn fragments_are_pieces_of_a_function_and_part_clones_are_not() {
        for piece in [
            "sqlite3VdbeExec.cold",
            "f.cold.3",
            "f.__part.1",
            "f.lto_priv.0",
        ] {
            assert!(is_fragment(piece), "{piece}");
        }
        for whole in ["f", "f.part.0", "f.isra.0", "f.constprop.1", "f.llvm.12"] {
            assert!(!is_fragment(whole), "{whole}");
        }
    }

    #[test]
    fn rust_symbols_are_told_by_their_mangling() {
        for rust in [
            "_RNvNtCs1_3foo3bar",
            "_ZN3foo3bar17h0123456789abcdefE",
            "_ZN3foo3bar17h0123456789abcdefE.llvm.77",
            "at:_RNvCs1_3foo3bar",
        ] {
            assert!(is_rust_symbol(rust), "{rust}");
        }
        for other in [
            "sqlite3VdbeExec",
            "_ZN3foo3barEv",
            "_ZN3foo3bar17hnothexnothexnotE",
            "main",
        ] {
            assert!(!is_rust_symbol(other), "{other}");
        }
    }

    #[test]
    fn portable_names_the_bench_binary_and_reduces_other_objects_to_paths() {
        let f = |ob: &str, sym: &str| (ob.to_string(), sym.to_string());
        let functions = vec![
            f("/lib/libc.so.6", "memcpy"),
            f("/lib/libc.so.6", "memmove"),
            f("/w/target/bench", "b.llvm.9"),
            f("/w/target/bench", "a'2"),
            f("/w/target/bench", "(below main)"),
            f("/w/target/bench", "0x0000000000023eb0"),
        ];
        assert_eq!(
            portable(Path::new("/w/target/bench"), &functions),
            [
                f("/lib/libc.so.6", ""),
                f(EXE, "0x0000000000023eb0"),
                f(EXE, "a"),
                f(EXE, "b"),
            ]
        );
    }

    #[test]
    fn an_address_takes_its_label_name_and_a_missing_one_has_none() {
        let mut binary = Binary::with_hashes(HashMap::from([("at:_init".to_string(), 1)]));
        binary
            .labels
            .insert("0x0000000000024000".into(), "at:_init".into());
        let named = binary
            .name_addresses(vec![(EXE.into(), "0x0000000000024000".into())])
            .expect("named");
        assert_eq!(named, [(EXE.to_string(), "at:_init".to_string())]);
        assert!(binary.code_key(&named).is_some());
        assert!(
            binary
                .name_addresses(vec![(EXE.into(), "0x0000000000000001".into())])
                .is_none()
        );
    }

    #[test]
    fn a_function_missing_from_the_binary_has_no_key() {
        let binary = Binary::with_hashes(HashMap::new());
        assert_eq!(binary.code_key(&[(EXE.into(), "gone".into())]), None);
    }

    #[test]
    fn hashing_a_function_of_the_running_test_binary() {
        let exe = std::env::current_exe().unwrap();
        let nm = command_text("nm", &["-S", "--defined-only"], &exe).unwrap();
        let name = nm
            .lines()
            .filter(|l| {
                l.split_whitespace()
                    .nth(2)
                    .is_some_and(|t| t == "t" || t == "T")
            })
            .filter_map(|l| l.split_whitespace().nth(3))
            .map(strip_llvm)
            .find(|n| n.starts_with("_R"))
            .unwrap()
            .to_string();
        let wanted = BTreeSet::from([name.clone()]);
        let functions = [(EXE.to_string(), name)];
        let first = Binary::open(&exe, &wanted, &[])
            .unwrap()
            .code_key(&functions);
        let second = Binary::open(&exe, &wanted, &[])
            .unwrap()
            .code_key(&functions);
        assert!(first.is_some());
        assert_eq!(first, second);
    }

    /// Every function of `bin`: sized ones by name, the crt helpers `nm`
    /// lists without a size by their `at:` label.
    fn functions(bin: &Path) -> BTreeSet<String> {
        let nm = command_text("nm", &["-S", "--defined-only"], bin).unwrap();
        nm.lines()
            .filter_map(|l| match l.split_whitespace().collect::<Vec<_>>()[..] {
                [_, _, "T" | "t", name] => Some(strip_llvm(name).into_owned()),
                [_, "T" | "t", name] if !name.ends_with("etext") => {
                    Some(format!("{LABELED}{name}"))
                }
                _ => None,
            })
            .collect()
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("soothfast-codekey-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn every_function_of_a_tiny_c_binary_hashes_under_each_linker_with_and_without_a_map() {
        let dir = scratch("c");
        let src = dir.join("t.c");
        std::fs::write(&src, "int main(void){return 0;}\n").unwrap();
        let mut built = 0;
        let mut unhashed = Vec::new();
        for linker in ["bfd", "lld", "mold"] {
            let bin = dir.join(linker);
            let map = dir.join(format!("{linker}.map"));
            let linked = Command::new("cc")
                .args(["-pie", "-O1", &format!("-fuse-ld={linker}"), "-o"])
                .arg(&bin)
                .arg(format!("-Wl,-Map={}", map.display()))
                .arg(&src)
                .output()
                .is_ok_and(|o| o.status.success());
            if !linked {
                continue;
            }
            built += 1;
            let wanted = functions(&bin);
            let text = std::fs::read_to_string(&map).expect("the linker wrote a map");
            let inputs = crate::linkmap::parse(&text, "").expect("the map parses");
            for (mapped, inputs) in [("no map", &[][..]), ("map", &inputs[..])] {
                let binary = Binary::open(&bin, &wanted, inputs);
                unhashed.extend(
                    wanted
                        .iter()
                        .filter(|n| !binary.as_ref().is_some_and(|b| b.hashes.contains_key(*n)))
                        .map(|n| format!("{linker} {mapped}: {n}")),
                );
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(built > 0, "no linker built a binary");
        assert!(unhashed.is_empty(), "unhashed: {unhashed:?}");
    }
}
