//! `[[bind]]` entries in `soothfast.toml`.
//!
//! Same skip-unknown-tables discipline as `sdk_config`: the file is shared
//! with the site, spec and SDK engines, so each parser reads only its own
//! tables.

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
    /// Target triples the package is built for.
    pub targets: Vec<String>,
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
            targets: Vec::new(),
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
    let mut in_bind = false;

    for (lineno, line) in logical_lines(text) {
        let line = line.as_str();
        if let Some(inner) = line.strip_prefix("[[").and_then(|l| l.strip_suffix("]]")) {
            in_bind = inner.trim() == "bind";
            if in_bind {
                cfg.entries.push(BindEntry::new());
            }
            continue;
        }
        if line.starts_with('[') {
            in_bind = false;
            continue;
        }
        if !in_bind {
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
        set(entry, key.trim(), value).map_err(|e| format!("line {lineno}: {e}"))?;
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
        ("targets", TomlValue::StrArray(a)) => entry.targets = a,
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
    fn one_source_surface_reaches_several_languages() {
        let cfg = parse(
            "[[bind]]\nlang = \"python\"\nout = \"py\"\npackage = \"acme-core\"\n\
             [[bind]]\nlang = \"wasm\"\nout = \"js\"\npackage = \"acme-core\"\n",
        )
        .expect("parses");
        let langs: Vec<&str> = cfg.entries.iter().map(|e| e.lang.name()).collect();
        assert_eq!(langs, vec!["python", "wasm"]);
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
    fn an_unknown_lang_lists_the_ones_that_exist() {
        let err =
            parse("[[bind]]\nout = \"o\"\npackage = \"p\"\nlang = \"go\"\n").expect_err("rejected");
        assert!(err.contains("go"), "{err}");
        assert!(err.contains("python"), "names the alternatives: {err}");
    }
}
