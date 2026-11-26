//! `CommonArgs` flag parsing and `--shard` item selection.

use std::sync::Once;

use crate::cpu::{self, CpuMask};

/// The cargo feature a package declares to compile its in-crate benches.
pub const INTERNALS_FEATURE: &str = "soothfast";

/// `features` with [`INTERNALS_FEATURE`] added when `declared`.
fn with_internals(features: Option<&str>, declared: bool) -> Option<String> {
    match features {
        Some(f) if declared && !f.split([',', ' ']).any(|name| name == INTERNALS_FEATURE) => {
            Some(format!("{f},{INTERNALS_FEATURE}"))
        }
        None if declared => Some(INTERNALS_FEATURE.to_string()),
        other => other.map(str::to_string),
    }
}

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
    /// `--cpu`: what callgrind hides from the measured process beyond
    /// AVX-512, in the form [`CpuMask::parse`] takes.
    pub cpu: Option<String>,
    /// `-j N` / `--jobs N`, handed to cargo for the bench build.
    pub jobs: Option<String>,
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
        let attached = arg.strip_prefix("--jobs=").or_else(|| {
            arg.strip_prefix("-j")
                .filter(|n| n.starts_with(|c: char| c.is_ascii_digit()))
        });
        if let Some(n) = attached {
            self.jobs = Some(n.to_string());
            return true;
        }
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
            "--cpu" => &mut self.cpu,
            "-j" | "--jobs" => &mut self.jobs,
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

impl CommonArgs {
    /// `features` plus [`INTERNALS_FEATURE`] when the package declares it in
    /// the tree at `dir`, which is what builds its in-crate benches.
    pub fn bench_features(&self, dir: Option<&std::path::Path>) -> Result<Option<String>, String> {
        let declared = match &self.pkg {
            Some(pkg) => crate::workspace::declares_feature(pkg, INTERNALS_FEATURE, dir)?,
            None => false,
        };
        Ok(with_internals(self.features.as_deref(), declared))
    }

    /// The mask `cpu` names, which is the default when unset.
    pub fn cpu_mask(&self) -> Result<CpuMask, String> {
        self.cpu
            .as_deref()
            .map_or(Ok(CpuMask::Default), CpuMask::parse)
            .map_err(|e| format!("--cpu: {e}"))
    }

    /// Check `cpu`, and drop it where the harness has no `hwcaps` tunable to
    /// apply it to, saying so once per process.
    pub fn settle_cpu(&mut self) -> Result<(), String> {
        self.settle_cpu_where(cpu::SUPPORTED)
    }

    fn settle_cpu_where(&mut self, supported: bool) -> Result<(), String> {
        static SAID: Once = Once::new();
        let mask = self.cpu_mask()?;
        if mask != CpuMask::Default && !supported {
            SAID.call_once(|| {
                println!(
                    "cpu = {mask}: ignored, glibc's hwcaps tunable exists only on x86-64 gnu targets"
                )
            });
            self.cpu = None;
        }
        Ok(())
    }

    /// What the mask adds to a measurement's identity, empty for the default.
    pub fn cpu_stamp(&self) -> String {
        self.cpu_mask().map(|m| m.identity()).unwrap_or_default()
    }
}

/// Codegen partitioning varies with unrelated edits: rustc reassigns modules
/// to units whenever a crate's contents change, which moves inlining and
/// register allocation in functions nobody touched. One unit cannot be
/// repartitioned, so both sides of a comparison are pinned to it.
const CANONICAL_CODEGEN_UNITS: &str = "1";

#[cfg(test)]
mod tests {
    use super::{CommonArgs, Shard, with_internals};
    use crate::cpu::CpuMask;

    #[test]
    fn jobs_parse_from_either_spelling() {
        for flag in ["-j", "--jobs"] {
            let argv = [flag.to_string(), "4".to_string()];
            let mut it = argv[1..].iter();
            let mut common = CommonArgs::default();
            assert!(common.try_parse(flag, &mut it));
            assert_eq!(common.jobs.as_deref(), Some("4"));
        }
        for attached in ["-j8", "--jobs=8"] {
            let mut common = CommonArgs::default();
            assert!(common.try_parse(attached, &mut [].iter()));
            assert_eq!(common.jobs.as_deref(), Some("8"));
        }
    }

    #[test]
    fn the_internals_feature_is_added_once_and_only_when_declared() {
        assert_eq!(with_internals(None, true).as_deref(), Some("soothfast"));
        assert_eq!(
            with_internals(Some("a"), true).as_deref(),
            Some("a,soothfast")
        );
        assert_eq!(
            with_internals(Some("a soothfast"), true).as_deref(),
            Some("a soothfast")
        );
        assert_eq!(with_internals(Some("a"), false).as_deref(), Some("a"));
        assert_eq!(with_internals(None, false), None);
    }

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

    #[test]
    fn the_cpu_flag_names_a_mask_and_an_unset_one_is_the_default() {
        let with = |cpu: Option<&str>| CommonArgs {
            cpu: cpu.map(String::from),
            ..CommonArgs::default()
        };
        assert_eq!(with(None).cpu_mask(), Ok(CpuMask::Default));
        assert_eq!(with(Some("portable")).cpu_mask(), Ok(CpuMask::Portable));
        assert!(with(None).cpu_mask().unwrap().hidden().is_empty());
        assert!(
            with(Some("AVX2,"))
                .cpu_mask()
                .unwrap_err()
                .starts_with("--cpu:")
        );
    }

    #[test]
    fn a_mask_is_dropped_where_there_is_no_tunable_and_stamps_as_the_default() {
        let mut common = CommonArgs {
            cpu: Some("portable".into()),
            ..CommonArgs::default()
        };
        common.settle_cpu_where(true).unwrap();
        assert_eq!(common.cpu.as_deref(), Some("portable"));
        common.settle_cpu_where(false).unwrap();
        assert_eq!(common.cpu, None);
        assert_eq!(common.cpu_stamp(), CommonArgs::default().cpu_stamp());
        let mut bad = CommonArgs {
            cpu: Some("AVX512F".into()),
            ..CommonArgs::default()
        };
        assert!(bad.settle_cpu_where(false).is_err());
    }
}
