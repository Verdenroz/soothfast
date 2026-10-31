//! A small image to hash in tests: two functions and a constant they read.

use std::collections::{BTreeSet, HashMap};
use std::io::Cursor;

use super::disasm::hash_functions;
use super::elf::{
    Marker, Reloc, Section, Symbol, addressable, parse_relocs, parse_sections, parse_symbols,
};
use super::resolve::Resolver;
use crate::linkmap::Extent;

pub const SECTIONS: &str = "\
Idx Name          Size      VMA               LMA               File off  Algn
  0 .text         00000100  0000000000001000  0000000000001000  00001000  2**4
                  CONTENTS, ALLOC, LOAD, READONLY, CODE
  1 .rodata       00000020  0000000000002000  0000000000002000  00002000  2**4
                  CONTENTS, ALLOC, LOAD, READONLY, DATA
  2 .tbss         00000008  0000000000003000  0000000000003000  00003000  2**3
                  ALLOC, THREAD_LOCAL
";

pub const SYMBOLS: &str = "\
0000000000001000 0000000000000020 T _ZN3foo3bar17h1E
0000000000001040 0000000000000020 t _ZN3foo3baz17h2E
0000000000002000 0000000000000008 r anon.abc.0.llvm.77
0000000000002010 r unsized
";

pub const FN_SYMBOLS: &str = "0000000000001000 0000000000000020 T _ZN3foo3bar17h1E\n";

pub const BAR: &str = "_ZN3foo3bar17h1E";
pub const BAZ: &str = "_ZN3foo3baz17h2E";

pub const DIS: &str = "\
0000000000001000 <_ZN3foo3bar17h1E>:
    1000:\tlea    0x1000(%rip),%rax        # 2000 <anon.abc.0.llvm.77>
    1007:\tcall   1040 <_ZN3foo3baz17h2E>
    100c:\tjne    1018 <_ZN3foo3bar17h1E+0x18>
    1012:\tmov    $0x2a,%eax
    1017:\tret
";

pub const OBJECTS: &str = "\
0000000000001000 0000000000000020 T _ZN3foo3bar17h1E
0000000000003000 0000000000000008 D one
0000000000003008 0000000000000008 D two
";

/// A function, a table at `0x2000`, and two 8-byte objects at `0x3000` in a
/// section with `flags`.
pub fn object_sections(flags: &str) -> String {
    format!(
        "\
Idx Name          Size      VMA               LMA               File off  Algn
  0 .text         00000100  0000000000001000  0000000000001000  00001000  2**4
                  CONTENTS, ALLOC, LOAD, READONLY, CODE
  1 .rodata       00000020  0000000000002000  0000000000002000  00002000  2**4
                  CONTENTS, ALLOC, LOAD, READONLY, DATA
  2 .objects      00000010  0000000000003000  0000000000003000  00003000  2**3
                  {flags}
"
    )
}

pub const WRITABLE: &str = "CONTENTS, ALLOC, LOAD, DATA";
pub const READ_ONLY: &str = "CONTENTS, ALLOC, LOAD, READONLY, DATA";

pub fn image(constant: u8, rest: u8) -> Vec<u8> {
    let mut image = vec![0; 0x3000];
    image[0x2000..0x2008].fill(constant);
    image[0x2008..0x2020].fill(rest);
    image
}

pub fn bar(h: &HashMap<String, u64>) -> Option<u64> {
    h.get(BAR).copied()
}

/// The sections of an `objdump -h` listing a resolver searches.
pub fn loaded(listing: &str) -> Vec<Section> {
    addressable(parse_sections(listing))
}

/// What a resolver is built from, [`SECTIONS`] and [`SYMBOLS`] with both
/// functions wanted unless replaced.
pub struct Fixture {
    sections: Vec<Section>,
    symbols: Vec<Symbol>,
    relocs: Vec<Reloc>,
    markers: HashMap<u64, Marker>,
    inputs: Vec<Extent>,
    wanted: BTreeSet<String>,
}

impl Default for Fixture {
    fn default() -> Fixture {
        Fixture {
            sections: loaded(SECTIONS),
            symbols: parse_symbols(SYMBOLS),
            relocs: Vec::new(),
            markers: HashMap::new(),
            inputs: Vec::new(),
            wanted: [BAR, BAZ].map(String::from).into(),
        }
    }
}

impl Fixture {
    pub fn sections(self, listing: &str) -> Fixture {
        Fixture {
            sections: loaded(listing),
            ..self
        }
    }

    pub fn symbols(self, listing: &str) -> Fixture {
        Fixture {
            symbols: parse_symbols(listing),
            ..self
        }
    }

    pub fn relocs(self, listing: &str) -> Fixture {
        Fixture {
            relocs: parse_relocs(listing),
            ..self
        }
    }

    pub fn markers(self, markers: HashMap<u64, Marker>) -> Fixture {
        Fixture { markers, ..self }
    }

    pub fn inputs(self, inputs: &[Extent]) -> Fixture {
        Fixture {
            inputs: inputs.to_vec(),
            ..self
        }
    }

    pub fn wanted(self, names: &[&str]) -> Fixture {
        Fixture {
            wanted: names.iter().map(|n| n.to_string()).collect(),
            ..self
        }
    }

    pub fn hashes(&self, image: &[u8], dis: &str) -> HashMap<String, u64> {
        self.labeled(image, dis).0
    }

    /// The hashes, and the `at:` name of each wanted address.
    pub fn labeled(
        &self,
        image: &[u8],
        dis: &str,
    ) -> (HashMap<String, u64>, HashMap<String, String>) {
        let resolver = Resolver::new(
            &self.sections,
            &self.symbols,
            &self.relocs,
            &self.markers,
            &self.inputs,
            image,
        );
        let mut labels = HashMap::new();
        let hashes = hash_functions(
            &resolver,
            &self.wanted,
            Cursor::new(dis.as_bytes()),
            &mut labels,
        );
        (hashes, labels)
    }
}
