//! Where a function's jumps out of its own span go, for the jump-table proof.

use super::elf::{Symbol, Target};
use super::is_fragment;
use super::resolve::Resolver;
use crate::jumptable::Exits;

/// What a slot read by `jmp *d(%rip)` is known to hold.
#[derive(Debug, PartialEq)]
pub(super) enum Slot<'a> {
    /// A dynamic relocation fills it with the address of this symbol.
    Named(&'a str),
    /// The address it holds: a relative relocation's target, or the bytes
    /// the link filled in.
    Address(u64),
    /// Nothing the file shows.
    Unknown,
}

/// Whether `target` is the entry of a function wholly outside `span` that is
/// no fragment of another. An unsized or non-code symbol at the address, or
/// no symbol at all, is a miss, and so is any alias that fails.
pub(super) fn entry_outside(symbols: &[Symbol], target: u64, span: (u64, u64)) -> bool {
    let first = symbols.partition_point(|s| s.addr < target);
    let at: Vec<&Symbol> = symbols[first..]
        .iter()
        .take_while(|s| s.addr == target)
        .collect();
    !at.is_empty()
        && at.iter().all(|s| {
            s.func && (s.addr + s.size <= span.0 || s.addr >= span.1) && !is_fragment(&s.name)
        })
}

/// Whether a tail call through `slot` leaves for good. A symbol this image
/// defines must pass `entry_outside`; one it does not define is external.
pub(super) fn slot_leaves(slot: Slot, symbols: &[Symbol], span: (u64, u64)) -> bool {
    match slot {
        Slot::Address(to) => entry_outside(symbols, to, span),
        Slot::Named(name) => {
            let mut defined = symbols.iter().filter(|s| s.name == name).peekable();
            defined.peek().is_none() || defined.all(|s| entry_outside(symbols, s.addr, span))
        }
        Slot::Unknown => false,
    }
}

impl Resolver<'_> {
    /// What the slot at `slot` holds, by its relocation, else by the 8 bytes
    /// in the file.
    fn slot(&self, slot: u64) -> Slot<'_> {
        match self.relocs.binary_search_by_key(&slot, |r| r.offset) {
            Ok(i) => match &self.relocs[i].target {
                Target::Symbol(name) => Slot::Named(name.split('@').next().unwrap_or(name)),
                Target::Relative(to) => Slot::Address(*to),
            },
            Err(_) => self
                .read(slot, 8)
                .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
                .map_or(Slot::Unknown, |bytes| {
                    Slot::Address(u64::from_le_bytes(bytes))
                }),
        }
    }

    /// Whether `addr` is a stub in the procedure linkage table.
    fn in_plt(&self, addr: u64) -> bool {
        self.section_at(addr)
            .is_some_and(|i| self.sections[i].name.starts_with(".plt"))
    }
}

/// The exits of one function: its span and whether it is Rust.
pub(super) struct FunctionExits<'a> {
    pub(super) resolver: &'a Resolver<'a>,
    pub(super) span: (u64, u64),
    pub(super) rust: bool,
}

impl Exits for FunctionExits<'_> {
    fn branch_leaves(&self, target: u64) -> bool {
        self.resolver.in_plt(target) || entry_outside(self.resolver.symbols, target, self.span)
    }

    fn slot_leaves(&self, slot: u64) -> bool {
        slot_leaves(self.resolver.slot(slot), self.resolver.symbols, self.span)
    }

    fn rust(&self) -> bool {
        self.rust
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(addr: u64, size: u64, name: &str, func: bool) -> Symbol {
        Symbol {
            addr,
            size,
            name: name.to_string(),
            func,
            local: false,
        }
    }

    const SPAN: (u64, u64) = (0x1000, 0x1100);

    fn symbols() -> Vec<Symbol> {
        vec![
            sym(0x1000, 0x100, "me", true),
            sym(0x2000, 0x40, "other", true),
            sym(0x2100, 0x40, "other.cold", true),
            sym(0x2200, 0x40, "table", false),
            sym(0x2300, 0x40, "clone.part.0", true),
        ]
    }

    #[test]
    fn a_relative_slot_leaves_only_for_the_entry_of_a_whole_outside_function() {
        let leaves = |to| slot_leaves(Slot::Address(to), &symbols(), SPAN);
        assert!(leaves(0x2000));
        assert!(leaves(0x2300), "a gcc .part clone is a function");
        assert!(!leaves(0x1000), "inside the span");
        assert!(!leaves(0x1040), "inside the span");
        assert!(!leaves(0x2100), "a .cold part");
        assert!(!leaves(0x2010), "the middle of a symbol");
        assert!(!leaves(0x2200), "not code");
        assert!(!leaves(0x3000), "an unsized or unknown address");
    }

    #[test]
    fn a_slot_with_no_relocation_is_read_from_the_file_or_a_miss() {
        assert!(slot_leaves(Slot::Address(0x2000), &symbols(), SPAN));
        assert!(!slot_leaves(Slot::Address(0x2010), &symbols(), SPAN));
        assert!(!slot_leaves(Slot::Address(0), &symbols(), SPAN));
        assert!(!slot_leaves(Slot::Unknown, &symbols(), SPAN));
    }

    #[test]
    fn a_named_slot_is_external_unless_the_image_defines_the_symbol() {
        let named = |name| slot_leaves(Slot::Named(name), &symbols(), SPAN);
        assert!(named("memcpy"), "undefined here, so external");
        assert!(named("other"));
        assert!(!named("me"), "defined in the span");
        assert!(!named("other.cold"));
        assert!(!named("table"));
    }

    #[test]
    fn an_alias_that_is_a_fragment_spoils_the_entry() {
        let mut aliased = symbols();
        aliased.push(sym(0x2000, 0x40, "other.cold", true));
        aliased.sort_by_key(|s| s.addr);
        assert!(!entry_outside(&aliased, 0x2000, SPAN));
    }
}
