//! `[[bind]]` entries in `soothfast.toml`.
//!
//! Same skip-unknown-tables discipline as `sdk_config`: the file is shared
//! with the site, spec and SDK engines, so each parser reads only its own
//! tables.

use std::collections::BTreeMap;
use std::path::Path;

use soothfast_bind::BindKind;
use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

/// One set of native bindings to generate, and where it goes.
///
/// What crosses over is decided by `#[soothfast::export]` in the source, so
/// an entry says only which language and where to put it.
#[derive(Debug, Clone)]
pub struct BindEntry {
    pub lang: BindKind,
    /// Output directory, relative to the package directory.
    pub out: String,
    /// Distribution name, e.g. `acme-core`.
    pub package: String,
    /// Import name; defaults to `package` with `-` replaced by `_`.
    pub module: Option<String>,
    /// Defaults to the crate version, keeping releases in lockstep.
    pub version: Option<String>,
    /// The binding library release the glue builds against. Defaults to
    /// whichever the backend was last verified against.
    pub backend_version: Option<String>,
    pub description: Option<String>,
    pub repository: Option<String>,
    /// Defaults to the crate's own authors.
    pub authors: Vec<String>,
    /// Target triples the package is built for.
    pub targets: Vec<String>,
    /// Python interpreters `bind build` produces a wheel for, one each;
    /// empty leaves the choice to maturin.
    pub interpreters: Vec<String>,
    /// `bind bench` script, relative to the package root: the same root
    /// `out` is relative to.
    pub bench: Option<String>,
    /// `[bind.types]`: a foreign type's canonical path, and how it crosses.
    /// The only mapping so far is `"str"`.
    pub types: BTreeMap<String, String>,
    /// Python only: also emit a `{name}_blocking` twin of every async call.
    pub blocking: bool,
}

impl BindEntry {
    fn new() -> Self {
        BindEntry {
            lang: BindKind::Python,
            out: String::new(),
            package: String::new(),
            module: None,
            version: None,
            backend_version: None,
            description: None,
            repository: None,
            authors: Vec::new(),
            targets: Vec::new(),
            interpreters: Vec::new(),
            bench: None,
            types: BTreeMap::new(),
            blocking: false,
        }
    }

    /// The import name, explicit or derived from the package name.
    pub fn module(&self) -> String {
        self.module
            .clone()
            .unwrap_or_else(|| self.package.replace('-', "_"))
    }
}

/// Every `[[bind]]` entry, in declaration order.
#[derive(Debug, Clone, Default)]
pub struct BindConfig {
    pub entries: Vec<BindEntry>,
}

/// Read `soothfast.toml` from a directory. An absent file just means no
/// bindings are configured.
pub fn load(dir: &Path) -> Result<BindConfig, String> {
    let path = dir.join("soothfast.toml");
    match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BindConfig::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
    }
}

/// Parse the `[[bind]]` sections of a `soothfast.toml`.
pub fn parse(text: &str) -> Result<BindConfig, String> {
    let mut cfg = BindConfig::default();
    let mut section = Section::Other;

    for (lineno, line) in logical_lines(text) {
        let line = line.as_str();
        if let Some(inner) = line.strip_prefix("[[").and_then(|l| l.strip_suffix("]]")) {
            section = Section::Other;
            if inner.trim() == "bind" {
                cfg.entries.push(BindEntry::new());
                section = Section::Bind;
            }
            continue;
        }
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            // `[bind.types]` is a sub-table of the preceding [[bind]] entry.
            section = match inner.trim() {
                "bind.types" if cfg.entries.is_empty() => {
                    return Err(format!(
                        "line {lineno}: [bind.types] before any [[bind]] entry"
                    ));
                }
                "bind.types" => Section::Types,
                _ => Section::Other,
            };
            continue;
        }
        if section == Section::Other {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {lineno}: expected `key = value`"));
        };
        let value = parse_value(value.trim()).map_err(|e| format!("line {lineno}: {e}"))?;
        let entry = cfg
            .entries
            .last_mut()
            .ok_or_else(|| format!("line {lineno}: key outside [[bind]]"))?;
        match section {
            Section::Bind => {
                set(entry, key.trim(), value).map_err(|e| format!("line {lineno}: {e}"))?
            }
            Section::Types => {
                let path = key.trim().trim_matches('"').to_string();
                let mapping =
                    type_mapping(&path, value).map_err(|e| format!("line {lineno}: {e}"))?;
                entry.types.insert(path, mapping);
            }
            Section::Other => {}
        }
    }

    for (i, e) in cfg.entries.iter().enumerate() {
        for (field, value) in [("out", &e.out), ("package", &e.package)] {
            if value.is_empty() {
                return Err(format!("[[bind]] entry {} has no `{field}`", i + 1));
            }
        }
    }
    Ok(cfg)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Bind,
    Types,
    Other,
}

/// One `[bind.types]` value. Only `"str"` exists so far: the type crosses
/// as a string through `Display` on the way out and `FromStr` on the way in.
fn type_mapping(path: &str, value: TomlValue) -> Result<String, String> {
    match value {
        TomlValue::Str(s) if s == "str" => Ok(s),
        _ => Err(format!(
            "[bind.types] entry {path:?}: only \"str\" (Display out, FromStr in) is supported"
        )),
    }
}

fn set(entry: &mut BindEntry, key: &str, value: TomlValue) -> Result<(), String> {
    match (key, value) {
        ("lang", TomlValue::Str(s)) => {
            entry.lang = BindKind::parse_name(&s).ok_or_else(|| {
                let known: Vec<&str> = BindKind::ALL.iter().map(|k| k.name()).collect();
                format!("unknown lang {s:?} (expected one of {})", known.join(", "))
            })?
        }
        ("out", TomlValue::Str(s)) => entry.out = s,
        ("package", TomlValue::Str(s)) => entry.package = s,
        ("module", TomlValue::Str(s)) => entry.module = Some(s),
        ("version", TomlValue::Str(s)) => entry.version = Some(s),
        ("backend_version", TomlValue::Str(s)) => entry.backend_version = Some(s),
        ("description", TomlValue::Str(s)) => entry.description = Some(s),
        ("repository", TomlValue::Str(s)) => entry.repository = Some(s),
        ("authors", TomlValue::StrArray(a)) => entry.authors = a,
        ("targets", TomlValue::StrArray(a)) => entry.targets = a,
        ("interpreters", TomlValue::StrArray(a)) => entry.interpreters = a,
        ("blocking", TomlValue::Bool(b)) => entry.blocking = b,
        ("bench", TomlValue::Str(s)) => entry.bench = Some(s),
        (key, _) => return Err(format!("unknown or mistyped `{key}`")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_entry_defaults_the_rest() {
        let cfg = parse("[[bind]]\nout = \"bindings/python\"\npackage = \"acme-core\"\n")
            .expect("parses");
        assert_eq!(cfg.entries.len(), 1);
        assert_eq!(cfg.entries[0].lang, BindKind::Python);
        assert_eq!(cfg.entries[0].module(), "acme_core");
    }

    #[test]
    fn blocking_is_a_bool_defaulting_to_false() {
        let cfg =
            parse("[[bind]]\nout = \"b/py\"\npackage = \"p\"\nblocking = true\n").expect("parses");
        assert!(cfg.entries[0].blocking);
        let cfg = parse("[[bind]]\nout = \"b/py\"\npackage = \"p\"\n").expect("parses");
        assert!(!cfg.entries[0].blocking);
        assert!(parse("[[bind]]\nout = \"b/py\"\npackage = \"p\"\nblocking = \"yes\"\n").is_err());
    }

    #[test]
    fn interpreters_is_a_list_of_strings() {
        let toml = "[[bind]]\nlang = \"python\"\nout = \"b/py\"\npackage = \"p\"\ninterpreters = [\"python3\", \"python3.14t\"]\n";
        let cfg = parse(toml).expect("parses");
        assert_eq!(cfg.entries[0].interpreters, vec!["python3", "python3.14t"]);
        let bad = "[[bind]]\nlang = \"python\"\nout = \"b/py\"\npackage = \"p\"\ninterpreters = \"python3\"\n";
        assert!(parse(bad).is_err());
    }

    #[test]
    fn a_types_table_maps_a_path_to_str() {
        let cfg = parse(
            "[[bind]]\nout = \"bindings/python\"\npackage = \"acme-core\"\n\n\
             [bind.types]\n\"chrono::DateTime\" = \"str\"\nUuid = \"str\"\n",
        )
        .expect("parses");
        let types = &cfg.entries[0].types;
        assert_eq!(
            types.get("chrono::DateTime").map(String::as_str),
            Some("str")
        );
        assert_eq!(types.get("Uuid").map(String::as_str), Some("str"));
    }

    #[test]
    fn a_types_value_other_than_str_is_an_error_naming_the_path() {
        let err = parse(
            "[[bind]]\nout = \"x\"\npackage = \"x\"\n\n[bind.types]\n\"chrono::DateTime\" = \"int\"\n",
        )
        .expect_err("rejects");
        assert!(err.contains("\"chrono::DateTime\""), "{err}");
        assert!(
            err.contains("only \"str\" (Display out, FromStr in) is supported"),
            "{err}"
        );
    }

    #[test]
    fn a_types_table_before_any_entry_is_an_error() {
        let err = parse("[bind.types]\n\"chrono::DateTime\" = \"str\"\n").expect_err("rejects");
        assert!(
            err.contains("[bind.types] before any [[bind]] entry"),
            "{err}"
        );
    }

    #[test]
    fn authors_is_a_list_of_strings() {
        let cfg = parse(
            "[[bind]]\nout = \"bindings/ruby\"\npackage = \"acme-core\"\n\
             authors = [\"Acme maintainers\"]\n",
        )
        .expect("parses");
        assert_eq!(cfg.entries[0].authors, vec!["Acme maintainers".to_string()]);
        assert!(parse("[[bind]]\nout = \"x\"\npackage = \"x\"\nauthors = \"me\"\n").is_err());
    }

    #[test]
    fn bench_names_a_script_relative_to_the_package_root() {
        let cfg = parse(
            "[[bind]]\nout = \"bindings/python\"\npackage = \"acme-core\"\n\
             bench = \"bindings/python/bench.py\"\n",
        )
        .expect("parses");
        assert_eq!(
            cfg.entries[0].bench.as_deref(),
            Some("bindings/python/bench.py")
        );
    }

    #[test]
    fn one_source_surface_reaches_several_languages() {
        let cfg = parse(
            "[[bind]]\nlang = \"python\"\nout = \"py\"\npackage = \"acme-core\"\n\
             [[bind]]\nlang = \"wasm\"\nout = \"js\"\npackage = \"acme-core\"\n\
             [[bind]]\nlang = \"node\"\nout = \"node\"\npackage = \"acme-core\"\n\
             [[bind]]\nlang = \"go\"\nout = \"go\"\npackage = \"github.com/acme/core\"\n",
        )
        .expect("parses");
        let langs: Vec<&str> = cfg.entries.iter().map(|e| e.lang.name()).collect();
        assert_eq!(langs, vec!["python", "wasm", "node", "go"]);
    }

    #[test]
    fn go_accepts_a_module_path_as_its_package() {
        let cfg =
            parse("[[bind]]\nlang = \"go\"\nout = \"go\"\npackage = \"github.com/acme/core\"\n")
                .expect("parses");
        assert_eq!(cfg.entries[0].lang, BindKind::Go);
        assert_eq!(cfg.entries[0].package, "github.com/acme/core");
    }

    #[test]
    fn tables_belonging_to_other_engines_are_skipped() {
        let cfg = parse(
            "[site]\nname = \"docs\"\n\
             [[sdk]]\nspec = \"openapi.yaml\"\nout = \"sdk\"\npackage = \"p\"\n\
             [[bind]]\nout = \"py\"\npackage = \"acme-core\"\n",
        )
        .expect("parses");
        assert_eq!(cfg.entries.len(), 1);
        assert_eq!(cfg.entries[0].package, "acme-core");
    }

    #[test]
    fn a_missing_required_key_is_rejected() {
        let err = parse("[[bind]]\nout = \"py\"\n").expect_err("package is required");
        assert!(err.contains("`package`"), "got {err}");
    }

    #[test]
    fn java_is_a_known_lang() {
        let cfg = parse("[[bind]]\nlang = \"java\"\nout = \"java\"\npackage = \"io.acme.core\"\n")
            .expect("parses");
        assert_eq!(cfg.entries[0].lang, BindKind::Java);
    }

    #[test]
    fn kotlin_is_a_known_lang() {
        let cfg =
            parse("[[bind]]\nlang = \"kotlin\"\nout = \"kotlin\"\npackage = \"io.acme.core\"\n")
                .expect("parses");
        assert_eq!(cfg.entries[0].lang, BindKind::Kotlin);
    }

    #[test]
    fn ruby_is_a_known_lang() {
        let cfg = parse("[[bind]]\nlang = \"ruby\"\nout = \"ruby\"\npackage = \"acme-core\"\n")
            .expect("parses");
        assert_eq!(cfg.entries[0].lang, BindKind::Ruby);
    }

    #[test]
    fn cpp_is_a_known_lang() {
        let cfg = parse("[[bind]]\nlang = \"cpp\"\nout = \"cpp\"\npackage = \"acme::core\"\n")
            .expect("parses");
        assert_eq!(cfg.entries[0].lang, BindKind::Cpp);
    }

    #[test]
    fn lua_accepts_a_dotted_require_path_as_its_package() {
        let cfg = parse("[[bind]]\nlang = \"lua\"\nout = \"lua\"\npackage = \"acme.core\"\n")
            .expect("parses");
        assert_eq!(cfg.entries[0].lang, BindKind::Lua);
        assert_eq!(cfg.entries[0].package, "acme.core");
    }

    #[test]
    fn luajit_is_accepted_as_an_alias_for_lua() {
        let cfg = parse("[[bind]]\nlang = \"luajit\"\nout = \"lua\"\npackage = \"acme\"\n")
            .expect("parses");
        assert_eq!(cfg.entries[0].lang, BindKind::Lua);
    }

    #[test]
    fn csharp_is_a_known_lang_with_its_short_forms() {
        for name in ["csharp", "cs", "dotnet"] {
            let cfg = parse(&format!(
                "[[bind]]\nlang = \"{name}\"\nout = \"csharp\"\npackage = \"Acme.Core\"\n"
            ))
            .expect("parses");
            assert_eq!(cfg.entries[0].lang, BindKind::CSharp);
        }
    }

    #[test]
    fn an_unknown_lang_lists_the_ones_that_exist() {
        let err = parse("[[bind]]\nout = \"o\"\npackage = \"p\"\nlang = \"cobol\"\n")
            .expect_err("rejected");
        assert!(err.contains("cobol"), "{err}");
        assert!(err.contains("python"), "names the alternatives: {err}");
    }
}
