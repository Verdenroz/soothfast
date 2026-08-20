//! What a measurement was compiled with.
//!
//! Instruction counts are only comparable between two builds that made the
//! same codegen decisions. The gate pins what it can (`codegen-units`) and
//! records the rest here, so a comparison across differing settings reports
//! that rather than reporting the settings as a regression.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use soothfast_site::toml::logical_lines;

/// The build settings behind one measurement run.
#[derive(Debug)]
pub struct BuildStamp {
    /// `rustc` release and host triple.
    pub rustc: String,
    /// What the gate pinned partitioning to, or `inherit`.
    pub codegen_units: String,
    /// Hash of the `[profile.*]` tables cargo will honour.
    pub profiles: String,
    /// Hash of the flags reaching rustc from the environment and config.
    pub rustflags: String,
}

impl BuildStamp {
    fn fields(&self) -> [(&'static str, &str); 4] {
        [
            ("rustc", &self.rustc),
            ("codegen_units", &self.codegen_units),
            ("profiles", &self.profiles),
            ("rustflags", &self.rustflags),
        ]
    }

    pub fn to_json(&self) -> Value {
        let mut doc = json!({});
        for (k, v) in self.fields() {
            doc[k] = json!(v);
        }
        doc
    }

    /// Short form for the gate banner.
    pub fn digest(&self) -> String {
        let joined = self
            .fields()
            .iter()
            .map(|(_, v)| *v)
            .collect::<Vec<_>>()
            .join("\u{1}");
        format!(
            "{:06x}",
            soothfast_registry::fnv1a(joined.as_bytes()) & 0xff_ffff
        )
    }
}

/// Capture the settings a bench build in `dir` (the current workspace when
/// `None`) will use, with `codegen_units` as the gate pinned it.
pub fn capture(codegen_units: Option<&str>, dir: Option<&Path>) -> BuildStamp {
    let root = match dir {
        Some(d) => Some(d.to_path_buf()),
        None => crate::invoke::workspace_root().ok(),
    };
    BuildStamp {
        rustc: rustc_version(dir),
        codegen_units: codegen_units.unwrap_or("inherit").to_string(),
        profiles: hash(&profile_lines(root.as_deref())),
        rustflags: hash(&rustflag_lines(root.as_deref())),
    }
}

/// Why two runs are not comparable.
pub struct Mismatch {
    /// Short form, repeated on every softened item.
    pub reason: String,
    /// Long form with the differing values, printed once.
    pub detail: String,
}

/// The reason two runs are not comparable, or `None` when they are.
pub fn compare(reference: &Value, current: &BuildStamp) -> Option<Mismatch> {
    let old = &reference["build"];
    if old.is_null() {
        return Some(Mismatch {
            reason: "reference predates build stamps".into(),
            detail: "reference predates build stamps; re-save it with `measure --save-baseline`"
                .into(),
        });
    }
    current.fields().into_iter().find_map(|(field, new)| {
        let was = old[field].as_str().unwrap_or_default();
        (was != new).then(|| Mismatch {
            reason: format!("build settings differ: {field}"),
            detail: format!("build settings differ: {field} ({was} -> {new})"),
        })
    })
}

fn rustc_version(dir: Option<&Path>) -> String {
    let mut cmd = Command::new("rustc");
    cmd.arg("-vV");
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let Ok(out) = cmd.output() else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name))
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    format!("{} {}", field("release:"), field("host:"))
        .trim()
        .to_string()
}

/// Cargo honours `[profile.*]` from the workspace root manifest and from
/// `.cargo/config.toml`; a member manifest's profiles are ignored.
fn profile_lines(root: Option<&Path>) -> Vec<String> {
    let Some(root) = root else {
        return Vec::new();
    };
    let mut out = tables_matching(&root.join("Cargo.toml"), |t| t.starts_with("profile"));
    out.extend(tables_matching(&cargo_config(root), |t| {
        t.starts_with("profile")
    }));
    out
}

fn rustflag_lines(root: Option<&Path>) -> Vec<String> {
    let Some(root) = root else {
        return Vec::new();
    };
    let mut out = tables_matching(&cargo_config(root), |t| {
        t == "build" || t.starts_with("target")
    });
    out.retain(|l| l.contains("rustflags") || l.contains("target") || l.contains("linker"));
    for var in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"] {
        if let Ok(v) = std::env::var(var) {
            out.push(format!("{var}={v}"));
        }
    }
    out.sort();
    out
}

fn cargo_config(root: &Path) -> PathBuf {
    let toml = root.join(".cargo").join("config.toml");
    if toml.exists() {
        return toml;
    }
    root.join(".cargo").join("config")
}

/// Every key under a table the predicate accepts, qualified by its table so
/// moving a key between tables reads as a change, and sorted so formatting
/// does not.
fn tables_matching(path: &Path, want: impl Fn(&str) -> bool) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut table = String::new();
    for (_, line) in logical_lines(&text) {
        if let Some(inner) = line
            .strip_prefix("[[")
            .and_then(|l| l.strip_suffix("]]"))
            .or_else(|| line.strip_prefix('[').and_then(|l| l.strip_suffix(']')))
        {
            table = inner.trim().to_string();
            continue;
        }
        if want(&table) {
            let key = line.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push(format!("{table}.{key}"));
        }
    }
    out.sort();
    out
}

fn hash(lines: &[String]) -> String {
    format!(
        "{:016x}",
        soothfast_registry::fnv1a(lines.join("\n").as_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(profiles: &str) -> BuildStamp {
        BuildStamp {
            rustc: "1.88.0 x86_64-unknown-linux-gnu".into(),
            codegen_units: "1".into(),
            profiles: profiles.into(),
            rustflags: "0".into(),
        }
    }

    #[test]
    fn matching_stamps_compare_clean() {
        let s = stamp("aaaa");
        assert!(compare(&json!({ "build": s.to_json() }), &s).is_none());
    }

    #[test]
    fn differing_profiles_name_the_field() {
        let reference = json!({ "build": stamp("aaaa").to_json() });
        let why = compare(&reference, &stamp("bbbb")).expect("mismatch");
        assert!(why.reason.contains("profiles"), "{}", why.reason);
        assert!(why.detail.contains("aaaa -> bbbb"), "{}", why.detail);
    }

    #[test]
    fn missing_stamp_asks_for_a_re_save() {
        let why = compare(&json!({ "items": {} }), &stamp("aaaa")).expect("mismatch");
        assert!(
            why.detail.contains("measure --save-baseline"),
            "{}",
            why.detail
        );
    }

    #[test]
    fn moving_a_key_between_tables_changes_the_hash() {
        let dir = std::env::temp_dir().join("soothfast-stamp-tables");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("Cargo.toml");

        std::fs::write(&path, "[profile.bench]\ncodegen-units = 1\n").unwrap();
        let bench = tables_matching(&path, |t| t.starts_with("profile"));
        std::fs::write(&path, "[profile.release]\ncodegen-units = 1\n").unwrap();
        let release = tables_matching(&path, |t| t.starts_with("profile"));

        assert_ne!(hash(&bench), hash(&release));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn formatting_does_not_change_the_hash() {
        let dir = std::env::temp_dir().join("soothfast-stamp-format");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("Cargo.toml");

        std::fs::write(&path, "[profile.bench]\ncodegen-units = 1\nlto = false\n").unwrap();
        let tight = tables_matching(&path, |t| t.starts_with("profile"));
        std::fs::write(
            &path,
            "# comment\n[profile.bench]\nlto   =   false\n\ncodegen-units   =   1\n",
        )
        .unwrap();
        let loose = tables_matching(&path, |t| t.starts_with("profile"));

        assert_eq!(hash(&tight), hash(&loose));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
