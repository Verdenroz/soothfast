//! Which build system `init` wires up, and the `[init]` table of
//! `soothfast.toml` that remembers the choice for `init --check`.

use std::path::Path;

use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

use crate::import::Format;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ecosystem {
    Rust,
    Gradle,
    Maven,
    Python,
    Node,
}

impl Ecosystem {
    pub fn parse(name: &str) -> Result<Ecosystem, String> {
        match name {
            "rust" => Ok(Ecosystem::Rust),
            "gradle" => Ok(Ecosystem::Gradle),
            "maven" => Ok(Ecosystem::Maven),
            "python" => Ok(Ecosystem::Python),
            "node" => Ok(Ecosystem::Node),
            other => Err(format!(
                "unknown ecosystem {other:?} (rust, gradle, maven, python or node)"
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Ecosystem::Rust => "rust",
            Ecosystem::Gradle => "gradle",
            Ecosystem::Maven => "maven",
            Ecosystem::Python => "python",
            Ecosystem::Node => "node",
        }
    }

    /// The first build file found at `root`, in precedence order.
    pub fn detect(root: &Path) -> Option<Ecosystem> {
        let has = |files: &[&str]| files.iter().any(|f| root.join(f).is_file());
        [
            (Ecosystem::Rust, &["Cargo.toml"][..]),
            (Ecosystem::Gradle, &["build.gradle.kts", "build.gradle"]),
            (Ecosystem::Maven, &["pom.xml"]),
            (Ecosystem::Python, &["pyproject.toml", "setup.py"]),
            (Ecosystem::Node, &["package.json"]),
        ]
        .into_iter()
        .find(|(_, files)| has(files))
        .map(|(eco, _)| eco)
    }

    /// The harness command and format used when nothing overrides them.
    /// Gradle has no command: the JMH plugin's output is configured per
    /// project.
    fn defaults(self) -> (Option<&'static str>, &'static str) {
        match self {
            Ecosystem::Rust | Ecosystem::Gradle => (None, "jmh"),
            Ecosystem::Maven => (
                Some(
                    "mvn -B -q -DskipTests package && \
                     java -jar target/benchmarks.jar -rf json -rff {out}",
                ),
                "jmh",
            ),
            Ecosystem::Python => (
                Some("uv run pytest --benchmark-only --benchmark-json {out}"),
                "pytest-benchmark",
            ),
            Ecosystem::Node => (Some("npx vitest bench --run --outputJson {out}"), "vitest"),
        }
    }
}

/// How a non-Rust repo's own harness is run and read. `{out}` in `cmd`
/// stands for `out`, relative to the checkout the command runs in.
#[derive(Debug, PartialEq)]
pub struct Bench {
    pub cmd: String,
    pub out: String,
    pub from: String,
}

/// The `[init]` table of `soothfast.toml`, which is also the shape of
/// init's flags: each field is set or left to the next layer down.
#[derive(Default, Debug, PartialEq)]
pub struct Settings {
    pub ecosystem: Option<Ecosystem>,
    pub bench_cmd: Option<String>,
    pub bench_out: Option<String>,
    pub from: Option<String>,
}

impl Settings {
    /// The `[init]` table of a `soothfast.toml`; other tables are skipped.
    pub fn parse(text: &str) -> Result<Settings, String> {
        let mut cfg = Settings::default();
        let mut in_init = false;
        for (lineno, line) in logical_lines(text) {
            if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                in_init = inner.trim() == "init";
                continue;
            }
            if !in_init {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("line {lineno}: expected `key = value`"));
            };
            let TomlValue::Str(value) =
                parse_value(value.trim()).map_err(|e| format!("line {lineno}: {e}"))?
            else {
                return Err(format!("line {lineno}: `{}` is a string", key.trim()));
            };
            match key.trim() {
                "ecosystem" => cfg.ecosystem = Some(Ecosystem::parse(&value)?),
                "bench_cmd" => cfg.bench_cmd = Some(value),
                "bench_out" => cfg.bench_out = Some(value),
                "from" => cfg.from = Some(value),
                k => return Err(format!("line {lineno}: unknown `{k}` under [init]")),
            }
        }
        Ok(cfg)
    }

    /// These settings, with `base` filling what they leave unset.
    pub fn over(self, base: Settings) -> Settings {
        Settings {
            ecosystem: self.ecosystem.or(base.ecosystem),
            bench_cmd: self.bench_cmd.or(base.bench_cmd),
            bench_out: self.bench_out.or(base.bench_out),
            from: self.from.or(base.from),
        }
    }

    /// The ecosystem these settings name, else the one `root` looks like.
    pub fn ecosystem(&self, root: &Path) -> Result<Ecosystem, String> {
        self.ecosystem
            .or_else(|| Ecosystem::detect(root))
            .ok_or_else(|| {
                "no Cargo.toml, build.gradle(.kts), pom.xml, pyproject.toml, setup.py or \
                 package.json here; pass --ecosystem"
                    .into()
            })
    }

    /// How `eco`'s harness runs, with each unset field defaulted.
    pub fn bench(&self, eco: Ecosystem) -> Result<Bench, String> {
        let (default_cmd, default_from) = eco.defaults();
        let cmd = match (&self.bench_cmd, default_cmd) {
            (Some(cmd), _) => cmd.clone(),
            (None, Some(cmd)) => cmd.into(),
            (None, None) => {
                return Err(format!(
                    "{} has no default benchmark command; pass --bench-cmd CMD and \
                     --bench-out PATH ({{out}} in CMD stands for PATH)",
                    eco.name()
                ));
            }
        };
        let from = self.from.clone().unwrap_or_else(|| default_from.into());
        Format::parse(&from)?;
        if !cmd.contains("{out}") {
            return Err("the benchmark command must write to {out}".into());
        }
        let out = self
            .bench_out
            .clone()
            .unwrap_or_else(|| "soothfast-bench.json".into());
        Ok(Bench { cmd, out, from })
    }
}

/// The `[init]` table that makes a later `init` render `bench` again.
pub fn section(eco: Ecosystem, bench: &Bench) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    format!(
        "[init]\necosystem = {}\nbench_cmd = {}\nbench_out = {}\nfrom = {}\n",
        quote(eco.name()),
        quote(&bench.cmd),
        quote(&bench.out),
        quote(&bench.from),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str, files: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("soothfast-eco-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            std::fs::write(dir.join(f), "").unwrap();
        }
        dir
    }

    #[test]
    fn each_build_file_names_its_ecosystem() {
        let cases = [
            ("Cargo.toml", Ecosystem::Rust),
            ("build.gradle.kts", Ecosystem::Gradle),
            ("build.gradle", Ecosystem::Gradle),
            ("pom.xml", Ecosystem::Maven),
            ("pyproject.toml", Ecosystem::Python),
            ("setup.py", Ecosystem::Python),
            ("package.json", Ecosystem::Node),
        ];
        for (file, eco) in cases {
            assert_eq!(Ecosystem::detect(&dir(file, &[file])), Some(eco), "{file}");
        }
        assert_eq!(Ecosystem::detect(&dir("none", &[])), None);
    }

    #[test]
    fn the_first_match_wins() {
        let all = ["package.json", "pyproject.toml", "pom.xml", "build.gradle"];
        assert_eq!(
            Ecosystem::detect(&dir("mixed", &all)),
            Some(Ecosystem::Gradle)
        );
        let with_cargo = ["package.json", "Cargo.toml"];
        assert_eq!(
            Ecosystem::detect(&dir("rust", &with_cargo)),
            Some(Ecosystem::Rust)
        );
    }

    #[test]
    fn a_flag_ecosystem_beats_detection() {
        let flags = Settings {
            ecosystem: Some(Ecosystem::Node),
            ..Settings::default()
        };
        assert_eq!(
            flags.ecosystem(&dir("flag", &["Cargo.toml"])),
            Ok(Ecosystem::Node)
        );
        assert!(Settings::default().ecosystem(&dir("empty", &[])).is_err());
        assert!(Ecosystem::parse("go").is_err());
    }

    #[test]
    fn defaults_fill_what_is_unset() {
        let b = Settings::default().bench(Ecosystem::Node).unwrap();
        assert_eq!(b.from, "vitest");
        assert_eq!(b.out, "soothfast-bench.json");
        assert_eq!(b.cmd, "npx vitest bench --run --outputJson {out}");
        let b = Settings::default().bench(Ecosystem::Maven).unwrap();
        assert!(b.cmd.contains("-rf json -rff {out}") && b.from == "jmh");
        let b = Settings::default().bench(Ecosystem::Python).unwrap();
        assert!(b.cmd.contains("--benchmark-json {out}") && b.from == "pytest-benchmark");
    }

    #[test]
    fn gradle_needs_its_command_spelled_out() {
        let e = Settings::default().bench(Ecosystem::Gradle).unwrap_err();
        assert!(
            e.contains("--bench-cmd") && e.contains("--bench-out"),
            "{e}"
        );
        let given = Settings {
            bench_cmd: Some("./gradlew jmh -Pout={out}".into()),
            bench_out: Some("build/jmh.json".into()),
            ..Settings::default()
        };
        let b = given.bench(Ecosystem::Gradle).unwrap();
        assert_eq!((b.out.as_str(), b.from.as_str()), ("build/jmh.json", "jmh"));
    }

    #[test]
    fn a_bad_format_or_a_command_without_out_is_an_error() {
        let from = Settings {
            from: Some("criterion".into()),
            ..Settings::default()
        };
        assert!(
            from.bench(Ecosystem::Node)
                .unwrap_err()
                .contains("criterion")
        );
        let cmd = Settings {
            bench_cmd: Some("npx vitest bench".into()),
            ..Settings::default()
        };
        assert!(cmd.bench(Ecosystem::Node).unwrap_err().contains("{out}"));
    }

    #[test]
    fn the_table_round_trips_and_other_tables_are_skipped() {
        let bench = Bench {
            cmd: "run \"x\" {out}".into(),
            out: "a\\b.json".into(),
            from: "vitest".into(),
        };
        let text = format!(
            "[site]\nname = \"x\"\n\n[gate]\nwalltime = \"advisory\"\n\n{}",
            section(Ecosystem::Node, &bench)
        );
        let cfg = Settings::parse(&text).unwrap();
        assert_eq!(cfg.ecosystem, Some(Ecosystem::Node));
        assert_eq!(cfg.bench(Ecosystem::Node).unwrap(), bench);
        assert_eq!(Settings::parse("[gate]\nx = 1\n"), Ok(Settings::default()));
    }

    #[test]
    fn an_unknown_init_key_is_an_error() {
        assert!(Settings::parse("[init]\nnope = \"x\"\n").is_err());
        assert!(Settings::parse("[init]\nfrom = 3\n").is_err());
    }

    #[test]
    fn flags_override_the_table() {
        let flags = Settings {
            bench_out: Some("f.json".into()),
            ..Settings::default()
        };
        let table = Settings {
            bench_out: Some("t.json".into()),
            from: Some("pyperf".into()),
            ..Settings::default()
        };
        let merged = flags.over(table);
        assert_eq!(merged.bench_out.as_deref(), Some("f.json"));
        assert_eq!(merged.from.as_deref(), Some("pyperf"));
    }
}
