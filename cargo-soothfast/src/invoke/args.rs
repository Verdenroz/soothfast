//! `CommonArgs` flag parsing and `--shard` item selection.

/// Flags shared by `measure` and `gate`.
#[derive(Default, Clone)]
pub struct CommonArgs {
    pub pkg: Option<String>,
    pub filter: Option<String>,
    pub backend: Option<String>,
    pub samples: Option<String>,
    pub features: Option<String>,
    /// `[[bench]]` target to link the registry from (default `"soothfast"`).
    pub target: Option<String>,
    /// `codegen-units` measurement builds are pinned to, or `"inherit"` to
    /// leave the project's own profile alone.
    pub codegen_units: Option<String>,
    /// `--no-item-cache`: measure every item even where a stored measurement
    /// of the same executed code exists.
    pub no_item_cache: bool,
    /// `--shard I/N`: measure and gate only the items this shard owns.
    pub shard: Option<Shard>,
}

/// One of `count` disjoint item sets, numbered from 1. An item belongs to the
/// shard its id hashes to, so an item that left the registry still has one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shard {
    pub index: u64,
    pub count: u64,
}

impl Shard {
    pub fn parse(s: &str) -> Result<Shard, String> {
        let bad = || format!("--shard wants I/N with 1 <= I <= N, got {s:?}");
        let (i, n) = s.split_once('/').ok_or_else(bad)?;
        let index: u64 = i.parse().map_err(|_| bad())?;
        let count: u64 = n.parse().map_err(|_| bad())?;
        if index == 0 || index > count {
            return Err(bad());
        }
        Ok(Shard { index, count })
    }

    pub fn owns(&self, id: &str) -> bool {
        soothfast_registry::fnv1a(id.as_bytes()) % self.count == self.index - 1
    }

    /// Whether a claim on `id` is this shard's to keep. One that names no
    /// item every shard keeps, and `baseline merge` keeps once.
    pub fn owns_claim(&self, id: &str) -> bool {
        id.is_empty() || self.owns(id)
    }
}

impl std::fmt::Display for Shard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.index, self.count)
    }
}

impl CommonArgs {
    /// Consume one flag if it belongs to the common set.
    pub fn try_parse<'a>(&mut self, arg: &str, it: &mut impl Iterator<Item = &'a String>) -> bool {
        let slot = match arg {
            "--no-item-cache" => {
                self.no_item_cache = true;
                return true;
            }
            "-p" | "--package" => &mut self.pkg,
            "--filter" => &mut self.filter,
            "--backend" => &mut self.backend,
            "--samples" => &mut self.samples,
            "--features" => &mut self.features,
            // `--bench` is the unambiguous spelling and works everywhere.
            // `--target` means the same thing here for compatibility, but
            // in `sdk build` it means a Rust triple the way cargo spells
            // it — which is why the other name had to exist.
            "--bench" | "--target" => &mut self.target,
            "--codegen-units" => &mut self.codegen_units,
            _ => return false,
        };
        *slot = it.next().cloned();
        true
    }

    /// The `codegen-units` to force on a measurement build, or `None` to
    /// inherit the project's profile.
    pub fn codegen_units_env(&self) -> Option<&str> {
        match self.codegen_units.as_deref() {
            Some("inherit") => None,
            Some(n) => Some(n),
            None => Some(CANONICAL_CODEGEN_UNITS),
        }
    }

    /// The pin as a build stamp records it: value and scope together, so a
    /// baseline taken under a different scope cannot read as comparable.
    pub fn codegen_units_stamp(&self) -> Option<String> {
        self.codegen_units_env().map(|n| format!("{n}/members"))
    }
}

/// Codegen partitioning varies with unrelated edits: rustc reassigns modules
/// to units whenever a crate's contents change, which moves inlining and
/// register allocation in functions nobody touched. One unit cannot be
/// repartitioned, so both sides of a comparison are pinned to it.
const CANONICAL_CODEGEN_UNITS: &str = "1";

#[cfg(test)]
mod tests {
    use super::Shard;

    #[test]
    fn a_shard_parses_from_one_and_rejects_out_of_range() {
        assert_eq!(Shard::parse("2/4"), Ok(Shard { index: 2, count: 4 }));
        for bad in ["0/4", "5/4", "1", "a/b", "1/0"] {
            assert!(Shard::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_item_belongs_to_exactly_one_shard() {
        let shards: Vec<Shard> = (1..=4).map(|index| Shard { index, count: 4 }).collect();
        for n in 0..200 {
            let id = format!("pkg::item_{n}");
            assert_eq!(shards.iter().filter(|s| s.owns(&id)).count(), 1, "{id}");
        }
    }
}
