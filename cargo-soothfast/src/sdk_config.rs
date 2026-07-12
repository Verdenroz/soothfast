//! `[[sdk]]` entries in `soothfast.toml`.
//!
//! Same skip-unknown-tables discipline as `spec_config`: the file is shared
//! with the site and spec engines, so each parser reads only its own tables.

use std::path::Path;

use soothfast_sdk::SdkKind;
use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

/// One SDK to generate, and where it goes.
#[derive(Debug, Clone)]
pub struct SdkEntry {
    /// The `[[spec]]` entry supplying operations. Must be generate-mode: an
    /// SDK derived from a hand-authored file would desynchronize from it.
    pub spec: String,
    pub lang: SdkKind,
    /// Output directory, relative to the package directory.
    pub out: String,
    /// Distribution name, e.g. `finance-query`.
    pub package: String,
    /// Import name; defaults to `package` with `-` replaced by `_`.
    pub module: Option<String>,
    /// Defaults to the crate version, keeping releases in lockstep.
    pub version: Option<String>,
    pub base_url: Option<String>,
    /// Server binary the SDK bundles and spawns, making the package
    /// self-contained. Without it the SDK is a client for a hosted API.
    pub embed: Option<String>,
    /// Arguments passed to the embedded server on every launch.
    pub embed_args: Vec<String>,
    /// Environment applied to every launch, over the ambient one. This is
    /// where a bundled server is told to bind loopback on an ephemeral
    /// port rather than wherever a deployment would put it.
    pub embed_env: std::collections::BTreeMap<String, String>,
    /// A dotenv template documenting what the embedded server reads,
    /// relative to the package directory. Its knobs become a typed
    /// `ServerEnv` on the generated client.
    pub embed_env_template: Option<String>,
    /// Target triples the embedded server is cross-compiled for.
    /// Empty means `Target::DEFAULT`.
    pub targets: Vec<String>,
    pub description: Option<String>,
    pub repository: Option<String>,
    /// Operation ids with opt-in cursor pagination.
    pub paginated: Vec<String>,
    pub cursor_param: Option<String>,
    pub limit_param: Option<String>,
}

impl SdkEntry {
    fn new() -> Self {
        SdkEntry {
            spec: String::new(),
            lang: SdkKind::Python,
            out: String::new(),
            package: String::new(),
            module: None,
            version: None,
            base_url: None,
            embed: None,
            embed_args: Vec::new(),
            embed_env: std::collections::BTreeMap::new(),
            embed_env_template: None,
            targets: Vec::new(),
            description: None,
            repository: None,
            paginated: Vec::new(),
            cursor_param: None,
            limit_param: None,
        }
    }

    /// The import name, explicit or derived from the package name.
    pub fn module(&self) -> String {
        self.module
            .clone()
            .unwrap_or_else(|| self.package.replace('-', "_"))
    }
}

/// Every `[[sdk]]` entry, in declaration order.
#[derive(Debug, Clone, Default)]
pub struct SdkConfig {
    pub entries: Vec<SdkEntry>,
}

/// Read `soothfast.toml` from a directory. An absent file just means no
/// SDKs are configured.
pub fn load(dir: &Path) -> Result<SdkConfig, String> {
    let path = dir.join("soothfast.toml");
    match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SdkConfig::default()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
    }
}

/// Parse the `[[sdk]]` sections of a `soothfast.toml`.
pub fn parse(text: &str) -> Result<SdkConfig, String> {
    let mut cfg = SdkConfig::default();
    let mut in_sdk = false;

    for (lineno, line) in logical_lines(text) {
        let line = line.as_str();
        if let Some(inner) = line.strip_prefix("[[").and_then(|l| l.strip_suffix("]]")) {
            in_sdk = inner.trim() == "sdk";
            if in_sdk {
                cfg.entries.push(SdkEntry::new());
            }
            continue;
        }
        if line.starts_with('[') {
            in_sdk = false;
            continue;
        }
        if !in_sdk {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {lineno}: expected `key = value`"));
        };
        let value = parse_value(value.trim()).map_err(|e| format!("line {lineno}: {e}"))?;
        let entry = cfg
            .entries
            .last_mut()
            .ok_or_else(|| format!("line {lineno}: key outside [[sdk]]"))?;
        set(entry, key.trim(), value).map_err(|e| format!("line {lineno}: {e}"))?;
    }

    for (i, e) in cfg.entries.iter().enumerate() {
        for (field, value) in [("spec", &e.spec), ("out", &e.out), ("package", &e.package)] {
            if value.is_empty() {
                return Err(format!("[[sdk]] entry {} has no `{field}`", i + 1));
            }
        }
    }
    Ok(cfg)
}

fn set(entry: &mut SdkEntry, key: &str, value: TomlValue) -> Result<(), String> {
    match (key, value) {
        ("spec", TomlValue::Str(s)) => entry.spec = s,
        ("lang", TomlValue::Str(s)) => {
            entry.lang = SdkKind::parse_name(&s).ok_or_else(|| {
                let known: Vec<&str> = SdkKind::ALL.iter().map(|k| k.name()).collect();
                format!("unknown lang {s:?} (expected one of {})", known.join(", "))
            })?
        }
        ("out", TomlValue::Str(s)) => entry.out = s,
        ("package", TomlValue::Str(s)) => entry.package = s,
        ("module", TomlValue::Str(s)) => entry.module = Some(s),
        ("version", TomlValue::Str(s)) => entry.version = Some(s),
        ("base_url", TomlValue::Str(s)) => entry.base_url = Some(s),
        ("embed", TomlValue::Str(s)) => entry.embed = Some(s),
        ("embed_args", TomlValue::StrArray(a)) => entry.embed_args = a,
        ("embed_env", TomlValue::Table(t)) => {
            for (name, value) in t {
                // Everything crossing an `environ` boundary is a string;
                // accepting `PORT = 0` unquoted only invites the mistake.
                let TomlValue::Str(value) = value else {
                    return Err(format!(
                        "embed_env.{name} must be a string — an environment \
                         holds no other type"
                    ));
                };
                entry.embed_env.insert(name, value);
            }
        }
        ("embed_env_template", TomlValue::Str(s)) => entry.embed_env_template = Some(s),
        ("targets", TomlValue::StrArray(a)) => entry.targets = a,
        ("description", TomlValue::Str(s)) => entry.description = Some(s),
        ("repository", TomlValue::Str(s)) => entry.repository = Some(s),
        ("paginated", TomlValue::StrArray(a)) => entry.paginated = a,
        ("cursor_param", TomlValue::Str(s)) => entry.cursor_param = Some(s),
        ("limit_param", TomlValue::Str(s)) => entry.limit_param = Some(s),
        (k, _) => return Err(format!("unknown or mistyped [[sdk]] key {k:?}")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_full_entry() {
        let cfg = parse(
            r#"
[[sdk]]
spec = "openapi.yaml"
lang = "python"
out = "../sdks/python"
package = "finance-query"
base_url = "https://api.example.com"
paginated = ["getQuotes", "getNews"]
"#,
        )
        .expect("parses");
        let e = &cfg.entries[0];
        assert_eq!(e.spec, "openapi.yaml");
        assert_eq!(e.lang, SdkKind::Python);
        assert_eq!(e.out, "../sdks/python");
        assert_eq!(e.package, "finance-query");
        assert_eq!(e.module(), "finance_query");
        assert_eq!(e.paginated.len(), 2);
    }

    #[test]
    fn an_embedded_server_is_read_with_its_arguments() {
        let cfg = parse(
            "[[sdk]]\nspec = \"openapi.yaml\"\nout = \"sdk\"\npackage = \"p\"\n\
             embed = \"acme-server\"\nembed_args = [\"serve\", \"--quiet\"]\n",
        )
        .expect("parses");
        let e = &cfg.entries[0];
        assert_eq!(e.embed.as_deref(), Some("acme-server"));
        assert_eq!(e.embed_args, ["serve", "--quiet"]);
        // A bundled server is what makes `base_url` optional.
        assert_eq!(e.base_url, None);
    }

    #[test]
    fn launch_environment_and_its_template_are_read_together() {
        let cfg = parse(
            "[[sdk]]\nspec = \"a.yaml\"\nout = \"o\"\npackage = \"p\"\n\
             embed = \"srv\"\nembed_env = { HOST = \"127.0.0.1\", PORT = \"0\" }\n\
             embed_env_template = \"../server/.env.template\"\n",
        )
        .expect("parses");
        let e = &cfg.entries[0];
        assert_eq!(e.embed_env["HOST"], "127.0.0.1");
        assert_eq!(e.embed_env["PORT"], "0");
        assert_eq!(
            e.embed_env_template.as_deref(),
            Some("../server/.env.template")
        );
    }

    /// `PORT = 0` is the natural typo, and it would reach `execve` as
    /// something that is not a string. Refuse it where it is written.
    #[test]
    fn a_non_string_launch_variable_is_refused() {
        let err = parse(
            "[[sdk]]\nspec = \"a.yaml\"\nout = \"o\"\npackage = \"p\"\n\
             embed = \"srv\"\nembed_env = { PORT = 0 }\n",
        )
        .expect_err("rejected");
        assert!(err.contains("PORT"), "{err}");
    }

    #[test]
    fn no_embed_key_leaves_the_sdk_a_plain_client() {
        let cfg =
            parse("[[sdk]]\nspec = \"a.yaml\"\nout = \"o\"\npackage = \"p\"\n").expect("parses");
        assert_eq!(cfg.entries[0].embed, None);
        assert!(cfg.entries[0].embed_args.is_empty());
    }

    #[test]
    fn other_engines_tables_are_skipped() {
        let cfg = parse(
            "[site]\nname = \"x\"\n\n[[spec]]\npath = \"openapi.yaml\"\n\n\
             [[sdk]]\nspec = \"openapi.yaml\"\nout = \"sdk\"\npackage = \"p\"\n",
        )
        .expect("parses");
        assert_eq!(cfg.entries.len(), 1);
    }

    #[test]
    fn a_missing_required_key_is_rejected() {
        let err = parse("[[sdk]]\nspec = \"openapi.yaml\"\nout = \"sdk\"\n")
            .expect_err("package is required");
        assert!(err.contains("`package`"), "got {err}");
    }

    #[test]
    fn an_unknown_lang_lists_the_ones_that_exist() {
        let err =
            parse("[[sdk]]\nspec = \"a.yaml\"\nout = \"o\"\npackage = \"p\"\nlang = \"go\"\n")
                .expect_err("rejected");
        assert!(err.contains("go"), "{err}");
        assert!(err.contains("python"), "names the alternatives: {err}");
    }

    #[test]
    fn an_unknown_key_is_rejected() {
        let err = parse("[[sdk]]\nspec = \"a\"\nout = \"o\"\npackage = \"p\"\nlagn = \"x\"\n")
            .expect_err("typo must not pass");
        assert!(err.contains("lagn"), "got {err}");
    }
}
