//! Which build systems `init` wires up, and the `[[project]]` tables of
//! `soothfast.toml` that `cargo soothfast plan` reads and `init --check`
//! compares against.

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

/// How a non-Rust project's own harness is run and read. `{out}` in `cmd`
/// stands for `out`, relative to the project's directory.
#[derive(Debug, PartialEq)]
pub struct Bench {
    pub cmd: String,
    pub out: String,
    pub from: String,
}

/// One `[[project]]` table, which is also the shape of init's flags: each
/// field is set or left to the next layer down. `walltime` and `prefix` are
/// only ever set in the file.
#[derive(Default, Debug, PartialEq, Clone)]
pub struct Settings {
    pub ecosystem: Option<Ecosystem>,
    pub bench_cmd: Option<String>,
    pub bench_out: Option<String>,
    pub from: Option<String>,
    pub walltime: Option<String>,
    pub prefix: Option<String>,
}

/// A saved project: its directory under the project root and its settings.
#[derive(Debug, PartialEq)]
pub struct Entry {
    pub dir: String,
    pub settings: Settings,
}

/// A non-Rust project the workflow gates.
#[derive(Debug, PartialEq)]
pub struct Project {
    pub ecosystem: Ecosystem,
    pub dir: String,
    pub bench: Bench,
    /// `hard` or `advisory`, over the `[gate]` setting for this project.
    pub walltime: Option<String>,
    /// The item-name prefix of the imported results.
    pub prefix: Option<String>,
}

/// The projects a `soothfast.toml` saves, by directory: each `[[project]]`
/// table, and the tables earlier releases wrote (`[[init.project]]`, and
/// `[init]` as the project at `.`).
pub fn parse_projects(text: &str) -> Result<Vec<Entry>, String> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut dir: Option<String> = None;
    let mut cfg: Option<Settings> = None;
    let mut finish = |dir: Option<String>, cfg: Option<Settings>| -> Result<(), String> {
        if let Some(settings) = cfg {
            let dir = dir.ok_or("[[init.project]] needs a `dir`")?;
            entries.push(Entry { dir, settings });
        }
        Ok(())
    };
    for (lineno, line) in logical_lines(text) {
        if let Some(name) = table_name(&line) {
            finish(dir.take(), cfg.take())?;
            match name {
                "init" => (dir, cfg) = (Some(".".into()), Some(Settings::default())),
                PROJECT_TABLE | LEGACY_PROJECT_TABLE => cfg = Some(Settings::default()),
                _ => {}
            }
            continue;
        }
        let Some(settings) = cfg.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {lineno}: expected `key = value`"));
        };
        let TomlValue::Str(value) =
            parse_value(value.trim()).map_err(|e| format!("line {lineno}: {e}"))?
        else {
            return Err(format!("line {lineno}: `{}` is a string", key.trim()));
        };
        match (key.trim(), dir.is_some()) {
            ("dir", false) => dir = Some(normalize_dir(&value)?),
            ("ecosystem", _) => settings.ecosystem = Some(Ecosystem::parse(&value)?),
            ("bench_cmd", _) => settings.bench_cmd = Some(value),
            ("bench_out", _) => settings.bench_out = Some(value),
            ("from", _) => settings.from = Some(value),
            ("walltime", _) if matches!(value.as_str(), "hard" | "advisory") => {
                settings.walltime = Some(value)
            }
            ("walltime", _) => {
                return Err(format!(
                    "line {lineno}: `walltime` is \"hard\" or \"advisory\""
                ));
            }
            ("prefix", _) => settings.prefix = Some(value),
            (k, _) => return Err(format!("line {lineno}: unknown `{k}` under [init]")),
        }
    }
    finish(dir, cfg)?;
    entries.sort_by(|a, b| a.dir.cmp(&b.dir));
    Ok(entries)
}

impl Settings {
    /// These settings, with `base` filling what they leave unset.
    pub fn over(self, base: Settings) -> Settings {
        Settings {
            ecosystem: self.ecosystem.or(base.ecosystem),
            bench_cmd: self.bench_cmd.or(base.bench_cmd),
            bench_out: self.bench_out.or(base.bench_out),
            from: self.from.or(base.from),
            walltime: self.walltime.or(base.walltime),
            prefix: self.prefix.or(base.prefix),
        }
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

/// The name in a `[name]` table header, ignoring padding and a trailing
/// comment. An array header `[[name]]` comes back as `[name]`.
pub fn table_name(line: &str) -> Option<&str> {
    let line = line.split('#').next()?.trim();
    Some(line.strip_prefix('[')?.strip_suffix(']')?.trim())
}

/// `[[project]]` as `table_name` reads it.
pub const PROJECT_TABLE: &str = "[project]";

/// `[[init.project]]` as `table_name` reads it.
pub const LEGACY_PROJECT_TABLE: &str = "[init.project]";

/// The `[[project]]` tables that make a later `init` render `projects`
/// again.
pub fn section(projects: &[Project]) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let tables: Vec<String> = projects
        .iter()
        .map(|p| {
            let mut table = format!(
                "[[project]]\ndir = {}\necosystem = {}\nbench_cmd = {}\nbench_out = {}\nfrom = {}\n",
                quote(&p.dir),
                quote(p.ecosystem.name()),
                quote(&p.bench.cmd),
                quote(&p.bench.out),
                quote(&p.bench.from),
            );
            for (key, value) in [("walltime", &p.walltime), ("prefix", &p.prefix)] {
                if let Some(value) = value {
                    table.push_str(&format!("{key} = {}\n", quote(value)));
                }
            }
            table
        })
        .collect();
    tables.join("\n")
}

/// `dir` as a path under the project root: `.` for the root itself, else
/// slash-separated with no `.` or `..` parts. Only characters that need no
/// quoting in the workflow's YAML and shell are allowed.
pub fn normalize_dir(dir: &str) -> Result<String, String> {
    let trimmed = dir.trim_start_matches("./").trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        return Ok(".".into());
    }
    let plain = trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c));
    let parts = trimmed
        .split('/')
        .all(|p| !p.is_empty() && p != "." && p != "..");
    if !plain || !parts {
        return Err(format!(
            "directory {dir:?} must be a relative path of letters, digits and . _ - /"
        ));
    }
    Ok(trimmed.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_names_ignore_padding_and_comments() {
        assert_eq!(table_name("[ init ] # managed"), Some("init"));
        assert_eq!(table_name("  [gate]"), Some("gate"));
        assert_eq!(table_name("[[project]]"), Some(PROJECT_TABLE));
        assert_eq!(table_name("[[init.project]]"), Some(LEGACY_PROJECT_TABLE));
        assert_eq!(table_name("walltime = \"hard\""), None);
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
        assert!(Ecosystem::parse("go").is_err());
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

    fn project(dir: &str, eco: Ecosystem, cmd: &str) -> Project {
        Project {
            ecosystem: eco,
            dir: dir.into(),
            bench: Bench {
                cmd: cmd.into(),
                out: "a\\b.json".into(),
                from: "vitest".into(),
            },
            walltime: None,
            prefix: None,
        }
    }

    #[test]
    fn the_tables_round_trip_and_other_tables_are_skipped() {
        let projects = [
            project("web", Ecosystem::Node, "run \"x\" {out}"),
            project(".", Ecosystem::Python, "py {out}"),
        ];
        let text = format!(
            "[site]\nname = \"x\"\n\n[gate]\nwalltime = \"advisory\"\n\n{}\n[site.extra]\ny = 1\n",
            section(&projects)
        );
        let entries = parse_projects(&text).unwrap();
        assert_eq!(
            entries.iter().map(|e| e.dir.as_str()).collect::<Vec<_>>(),
            [".", "web"]
        );
        let web = &entries[1];
        assert_eq!(web.settings.ecosystem, Some(Ecosystem::Node));
        assert_eq!(
            web.settings.bench(Ecosystem::Node).unwrap(),
            projects[0].bench
        );
        assert_eq!(parse_projects("[gate]\nx = 1\n"), Ok(Vec::new()));
    }

    #[test]
    fn walltime_and_prefix_survive_a_round_trip() {
        let mut p = project("web", Ecosystem::Node, "run {out}");
        p.walltime = Some("advisory".into());
        p.prefix = Some("web".into());
        let entry = parse_projects(&section(&[p])).unwrap().remove(0);
        assert_eq!(entry.settings.walltime.as_deref(), Some("advisory"));
        assert_eq!(entry.settings.prefix.as_deref(), Some("web"));
        let bad = "[[project]]\ndir = \"x\"\nwalltime = \"soft\"\n";
        assert!(parse_projects(bad).is_err());
    }

    #[test]
    fn the_init_project_tables_are_read_as_projects() {
        let text = "[[init.project]]\ndir = \"web\"\necosystem = \"node\"\n";
        let entries = parse_projects(text).unwrap();
        assert_eq!(entries[0].dir, "web");
    }

    #[test]
    fn a_legacy_init_table_is_the_project_at_the_root() {
        let entries = parse_projects("[init]\necosystem = \"node\"\nfrom = \"vitest\"\n").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].dir, ".");
        assert_eq!(entries[0].settings.ecosystem, Some(Ecosystem::Node));
        assert!(parse_projects("[init]\ndir = \"x\"\n").is_err());
    }

    #[test]
    fn a_bad_table_is_an_error() {
        assert!(parse_projects("[init]\nnope = \"x\"\n").is_err());
        assert!(parse_projects("[init]\nfrom = 3\n").is_err());
        assert!(parse_projects("[[project]]\necosystem = \"node\"\n").is_err());
        assert!(parse_projects("[[project]]\ndir = \"../x\"\n").is_err());
    }

    #[test]
    fn directories_are_normalised_or_rejected() {
        assert_eq!(normalize_dir("./client/"), Ok("client".into()));
        assert_eq!(normalize_dir(""), Ok(".".into()));
        assert_eq!(normalize_dir("a/b-c_d.e"), Ok("a/b-c_d.e".into()));
        for bad in ["/abs", "a/../b", "a//b", "a b", "a\"b", "$x"] {
            assert!(normalize_dir(bad).is_err(), "{bad}");
        }
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
