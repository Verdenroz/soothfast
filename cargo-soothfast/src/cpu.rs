//! The CPU features callgrind hides from the measured process, set by `cpu`
//! under `[gate]` and a package's tables, or by `--cpu`.
//!
//! The harness always hides AVX-512, which valgrind cannot run. A mask hides
//! more, so glibc picks the same memory and string routines on every host.

use std::fmt;

/// Names glibc 2.42 takes in `glibc.cpu.hwcaps` (`sysdeps/x86/cpu-tunables.c`):
/// the CPU features above x86-64-v2 that an ifunc or the lazy-binding
/// trampoline selects on, then the preferred bits their selectors read, which
/// glibc sets per CPU model. An older glibc ignores the names it lacks.
const PORTABLE: &[&str] = &[
    "AVX",
    "AVX2",
    "BMI1",
    "BMI2",
    "ERMS",
    "FMA",
    "FMA4",
    "LZCNT",
    "MOVBE",
    "RTM",
    "XSAVEC",
    "Avoid_Non_Temporal_Memset",
    "Avoid_STOSB",
    "AVX_Fast_Unaligned_Load",
    "Fast_Copy_Backward",
    "Fast_Rep_String",
    "Fast_Unaligned_Copy",
    "Fast_Unaligned_Load",
    "MathVec_Prefer_No_AVX512",
    "Prefer_ERMS",
    "Prefer_FSRM",
    "Prefer_No_AVX512",
    "Prefer_No_VZEROUPPER",
    "Prefer_PMINUB_for_stringop",
    "Slow_BSF",
    "Slow_SSE4_2",
];

/// Whether glibc's `hwcaps` tunable exists where the harness runs.
pub const SUPPORTED: bool = cfg!(all(target_arch = "x86_64", target_env = "gnu"));

/// What to hide beyond AVX-512.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CpuMask {
    /// Nothing more than AVX-512.
    #[default]
    Default,
    /// Everything above x86-64-v2 that glibc dispatches on.
    Portable,
    /// These names, as spelled in glibc's `hwcaps`.
    Hide(Vec<String>),
}

impl CpuMask {
    /// `default`, `portable`, or a comma-separated list of names.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "default" => Ok(CpuMask::Default),
            "portable" => Ok(CpuMask::Portable),
            list => Self::hide(list.split(',').map(str::to_string).collect()),
        }
    }

    /// A list of feature names. An empty one is the default mask.
    pub fn hide(names: Vec<String>) -> Result<Self, String> {
        if names.is_empty() {
            return Ok(CpuMask::Default);
        }
        for name in &names {
            let word =
                !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            if !word {
                return Err(format!(
                    "cpu feature {name:?} must be letters, digits and underscores"
                ));
            }
            if name.to_ascii_uppercase().starts_with("AVX512") {
                return Err(format!(
                    "cpu feature {name:?}: AVX-512 is always hidden, valgrind cannot run it"
                ));
            }
            if name == "default" || name == "portable" {
                return Err(format!("cpu feature {name:?} is a preset, not a feature"));
            }
        }
        Ok(CpuMask::Hide(names))
    }

    /// The names hidden beyond AVX-512, sorted and without repeats. Empty
    /// for the default mask.
    pub fn hidden(&self) -> Vec<String> {
        let mut names: Vec<String> = match self {
            CpuMask::Default => Vec::new(),
            CpuMask::Portable => PORTABLE.iter().map(|n| n.to_string()).collect(),
            CpuMask::Hide(names) => names.clone(),
        };
        names.sort();
        names.dedup();
        names
    }

    /// What the mask adds to a measurement's identity. Empty under the
    /// default, which keys like a run with no mask.
    pub fn identity(&self) -> String {
        self.hidden().join(",")
    }
}

impl fmt::Display for CpuMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CpuMask::Default => f.write_str("default"),
            CpuMask::Portable => f.write_str("portable"),
            CpuMask::Hide(names) => f.write_str(&names.join(",")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_forms_parse() {
        assert_eq!(CpuMask::parse("default"), Ok(CpuMask::Default));
        assert_eq!(CpuMask::parse("portable"), Ok(CpuMask::Portable));
        assert_eq!(
            CpuMask::parse("AVX2,ERMS"),
            Ok(CpuMask::Hide(vec!["AVX2".into(), "ERMS".into()]))
        );
    }

    #[test]
    fn an_empty_list_is_the_default_mask() {
        assert_eq!(CpuMask::hide(Vec::new()), Ok(CpuMask::Default));
    }

    #[test]
    fn a_name_outside_the_character_set_is_rejected() {
        for bad in [
            "",
            "AVX2,",
            "-AVX2",
            "AVX2:glibc.malloc.check=1",
            "A B",
            "AVX,é",
        ] {
            assert!(CpuMask::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn avx512_and_preset_names_are_not_features_to_list() {
        for bad in ["AVX512F", "avx512vl", "AVX2,AVX512_VBMI", "portable,AVX2"] {
            assert!(CpuMask::parse(bad).is_err(), "{bad:?}");
        }
        assert!(CpuMask::hide(vec!["default".into()]).is_err());
    }

    #[test]
    fn the_default_hides_nothing_beyond_avx512_and_adds_no_identity() {
        assert!(CpuMask::Default.hidden().is_empty());
        assert_eq!(CpuMask::Default.identity(), "");
    }

    #[test]
    fn portable_hides_what_glibc_dispatches_above_x86_64_v2() {
        let hidden = CpuMask::Portable.hidden();
        for name in [
            "AVX", "AVX2", "BMI2", "ERMS", "FMA", "LZCNT", "MOVBE", "RTM",
        ] {
            assert!(hidden.iter().any(|n| n == name), "{name}");
        }
        for kept in ["SSE2", "SSSE3", "SSE4_1", "SSE4_2", "POPCNT", "CMOV", "CX8"] {
            assert!(!hidden.iter().any(|n| n == kept), "{kept}");
        }
    }

    #[test]
    fn a_list_is_sorted_and_deduplicated_for_its_identity() {
        let one = CpuMask::parse("ERMS,AVX2,ERMS").unwrap();
        let two = CpuMask::parse("AVX2,ERMS").unwrap();
        assert_eq!(one.identity(), "AVX2,ERMS");
        assert_eq!(one.identity(), two.identity());
    }

    #[test]
    fn a_mask_prints_in_the_form_that_parses_back() {
        for text in ["default", "portable", "AVX2,ERMS"] {
            assert_eq!(CpuMask::parse(text).unwrap().to_string(), text);
        }
    }
}
