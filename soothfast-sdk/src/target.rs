//! Rust target triples → the platform metadata each packaging format needs.
//!
//! npm and Python describe the same machine in different vocabularies —
//! `os`/`cpu`/`libc` triples on one side, wheel platform tags on the other.
//! Both are derived here so the emitters and the build command cannot
//! disagree about what `aarch64-apple-darwin` means.

/// One cross-compilation target and how each ecosystem names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// Rust target triple, as `cargo build --target` takes it.
    pub triple: &'static str,
    /// npm `os` field.
    pub os: &'static str,
    /// npm `cpu` field.
    pub cpu: &'static str,
    /// npm `libc` field; only Linux distinguishes.
    pub libc: Option<&'static str>,
    /// Python wheel platform tag.
    ///
    /// A `manylinux_*` tag is a claim about the *build* environment, not
    /// about this table: it holds only when the binary was linked against
    /// a glibc at least that old. `sdk build` warns when it cannot tell
    /// that it was.
    pub wheel_platform: &'static str,
    /// Executable suffix on this target.
    pub exe_suffix: &'static str,
}

impl Target {
    /// Every target the table knows how to describe.
    pub const ALL: &'static [Target] = &[
        Target {
            triple: "aarch64-apple-darwin",
            os: "darwin",
            cpu: "arm64",
            libc: None,
            wheel_platform: "macosx_11_0_arm64",
            exe_suffix: "",
        },
        Target {
            triple: "aarch64-pc-windows-msvc",
            os: "win32",
            cpu: "arm64",
            libc: None,
            wheel_platform: "win_arm64",
            exe_suffix: ".exe",
        },
        Target {
            triple: "aarch64-unknown-linux-gnu",
            os: "linux",
            cpu: "arm64",
            libc: Some("glibc"),
            wheel_platform: "manylinux_2_17_aarch64",
            exe_suffix: "",
        },
        Target {
            triple: "aarch64-unknown-linux-musl",
            os: "linux",
            cpu: "arm64",
            libc: Some("musl"),
            wheel_platform: "musllinux_1_2_aarch64",
            exe_suffix: "",
        },
        Target {
            triple: "x86_64-apple-darwin",
            os: "darwin",
            cpu: "x64",
            libc: None,
            wheel_platform: "macosx_10_12_x86_64",
            exe_suffix: "",
        },
        Target {
            triple: "x86_64-pc-windows-msvc",
            os: "win32",
            cpu: "x64",
            libc: None,
            wheel_platform: "win_amd64",
            exe_suffix: ".exe",
        },
        Target {
            triple: "x86_64-unknown-linux-gnu",
            os: "linux",
            cpu: "x64",
            libc: Some("glibc"),
            wheel_platform: "manylinux_2_17_x86_64",
            exe_suffix: "",
        },
        Target {
            triple: "x86_64-unknown-linux-musl",
            os: "linux",
            cpu: "x64",
            libc: Some("musl"),
            wheel_platform: "musllinux_1_2_x86_64",
            exe_suffix: "",
        },
    ];

    /// The matrix used when an `[[sdk]]` entry names no `targets`: the
    /// platforms a published package is expected to cover. musl is not in
    /// it — it needs its own toolchain and most consumers never ask.
    pub const DEFAULT: &'static [&'static str] = &[
        "aarch64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "x86_64-pc-windows-msvc",
        "x86_64-unknown-linux-gnu",
    ];

    /// Look a triple up in the table.
    pub fn find(triple: &str) -> Option<&'static Target> {
        Target::ALL.iter().find(|t| t.triple == triple)
    }

    /// The configured targets, or the default matrix when none are named.
    /// Sorted and deduplicated, so emission stays deterministic.
    pub fn matrix(configured: &[String]) -> Result<Vec<&'static Target>, String> {
        let names: Vec<&str> = if configured.is_empty() {
            Target::DEFAULT.to_vec()
        } else {
            configured.iter().map(String::as_str).collect()
        };
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let target = Target::find(name).ok_or_else(|| {
                let known: Vec<&str> = Target::ALL.iter().map(|t| t.triple).collect();
                format!(
                    "unknown target {name:?} (expected one of {})",
                    known.join(", ")
                )
            })?;
            if !out.contains(&target) {
                out.push(target);
            }
        }
        out.sort_by_key(|t| t.triple);
        Ok(out)
    }

    /// The npm sub-package suffix: `linux-x64`, `darwin-arm64`,
    /// `linux-x64-musl`. Mirrors what esbuild and swc publish under.
    pub fn npm_suffix(&self) -> String {
        match self.libc {
            Some("musl") => format!("{}-{}-musl", self.os, self.cpu),
            _ => format!("{}-{}", self.os, self.cpu),
        }
    }

    /// The full wheel tag hatchling wants.
    pub fn wheel_tag(&self) -> String {
        format!("py3-none-{}", self.wheel_platform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_every_triple_is_distinct() {
        let triples: Vec<&str> = Target::ALL.iter().map(|t| t.triple).collect();
        let mut sorted = triples.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(triples, sorted, "keep ALL sorted and unique");
    }

    #[test]
    fn the_default_matrix_is_all_resolvable() {
        for triple in Target::DEFAULT {
            assert!(Target::find(triple).is_some(), "{triple}");
        }
    }

    #[test]
    fn npm_suffixes_match_what_the_ecosystem_publishes() {
        let suffix = |t: &str| Target::find(t).expect("known").npm_suffix();
        assert_eq!(suffix("x86_64-unknown-linux-gnu"), "linux-x64");
        assert_eq!(suffix("aarch64-apple-darwin"), "darwin-arm64");
        assert_eq!(suffix("x86_64-pc-windows-msvc"), "win32-x64");
        assert_eq!(suffix("x86_64-unknown-linux-musl"), "linux-x64-musl");
    }

    #[test]
    fn glibc_and_musl_linux_never_collide() {
        let gnu = Target::find("x86_64-unknown-linux-gnu").expect("known");
        let musl = Target::find("x86_64-unknown-linux-musl").expect("known");
        assert_ne!(gnu.npm_suffix(), musl.npm_suffix());
        assert_ne!(gnu.wheel_tag(), musl.wheel_tag());
    }

    #[test]
    fn an_empty_matrix_falls_back_to_the_default() {
        let matrix = Target::matrix(&[]).expect("defaults resolve");
        assert_eq!(matrix.len(), Target::DEFAULT.len());
    }

    #[test]
    fn a_repeated_target_is_only_built_once() {
        let matrix = Target::matrix(&[
            "x86_64-unknown-linux-gnu".into(),
            "x86_64-unknown-linux-gnu".into(),
        ])
        .expect("resolves");
        assert_eq!(matrix.len(), 1);
    }

    #[test]
    fn an_unknown_target_lists_the_ones_that_exist() {
        let err = Target::matrix(&["sparc-unknown-none".into()]).expect_err("rejected");
        assert!(err.contains("sparc"), "{err}");
        assert!(err.contains("x86_64-unknown-linux-gnu"), "{err}");
    }
}
