//! The `[profile.dev]` debuginfo advice: line tables for the workspace and
//! none for dependencies. `test` inherits `dev`; `release` and `bench` are
//! never touched, so what the perf gate measures cannot move.

use std::path::Path;

const DEBUG: &str = "debug = \"line-tables-only\"\n";
const PACKAGES: &str = "[profile.dev.package.\"*\"]\ndebug = false\n";

/// `text` with the dev profile added, or `None` when it already sets `debug`
/// for `dev` or has a `dev` dependency override of its own.
fn with_dev_profile(text: &str) -> Option<String> {
    let mut table = "";
    let mut header: Option<usize> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let code = line.split('#').next().unwrap_or("").trim();
        if let Some(name) = code.strip_prefix('[').and_then(|c| c.strip_suffix(']')) {
            if !name.starts_with('[') {
                table = name.trim();
                match table {
                    "profile.dev" => header = Some(offset + line.len()),
                    "profile.dev.package.\"*\"" => return None,
                    _ => {}
                }
            }
        } else if let Some((key, _)) = code.split_once('=') {
            let key = key.trim();
            let dev_debug = table == "profile.dev" && key == "debug";
            let dotted = table.is_empty() && key.starts_with("profile.dev");
            if dev_debug || dotted {
                return None;
            }
        }
        offset += line.len();
    }
    let mut out = text.to_string();
    if let Some(at) = header {
        out.insert_str(at, DEBUG);
    } else {
        let gap = if out.is_empty() || out.ends_with("\n\n") {
            ""
        } else if out.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        out.push_str(&format!("{gap}[profile.dev]\n{DEBUG}"));
    }
    out.push_str(&format!("\n{PACKAGES}"));
    Some(out)
}

/// Advice for the workspace manifest at `root`, or the edit itself when
/// `write` is set. `None` when the profile is already set or not needed.
pub(super) fn dev_profile(root: &Path, write: bool) -> Result<Option<String>, String> {
    let path = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let Some(edited) = with_dev_profile(&text) else {
        return Ok(None);
    };
    if !write {
        return Ok(Some(
            "advice   Cargo.toml: [profile.dev] debug = \"line-tables-only\" and \
             [profile.dev.package.\"*\"] debug = false build and test faster; \
             --dev-profile writes it, and release and bench are untouched"
                .into(),
        ));
    }
    std::fs::write(&path, edited).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(format!("wrote    {} ([profile.dev])", path.display())))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::scratch;
    use super::*;

    const RELEASE: &str = "[profile.release]\nlto = true\n\n[profile.bench]\ndebug = true\n\n[profile.test]\nopt-level = 1\n";

    #[test]
    fn a_manifest_without_a_dev_profile_gets_both_tables() {
        let text = "[workspace]\nmembers = [\"a\"]\n";
        let out = with_dev_profile(text).unwrap();
        assert_eq!(
            out,
            "[workspace]\nmembers = [\"a\"]\n\n[profile.dev]\ndebug = \"line-tables-only\"\n\n\
             [profile.dev.package.\"*\"]\ndebug = false\n"
        );
    }

    #[test]
    fn an_existing_dev_table_gains_the_key_in_place() {
        let text = "[profile.dev]\nopt-level = 1\n\n[profile.release]\nlto = true\n";
        let out = with_dev_profile(text).unwrap();
        assert!(out.starts_with("[profile.dev]\ndebug = \"line-tables-only\"\nopt-level = 1\n"));
        assert!(out.contains("\n[profile.release]\nlto = true\n"));
        assert_eq!(out.matches("[profile.dev]").count(), 1);
    }

    #[test]
    fn release_bench_and_test_profiles_are_untouched() {
        let text = format!("[workspace]\n\n{RELEASE}");
        let out = with_dev_profile(&text).unwrap();
        assert!(out.starts_with(&text));
        assert!(out.contains(RELEASE));
        let added = &out[text.len()..];
        assert!(!added.contains("release") && !added.contains("bench"));
        assert!(!added.contains("profile.test"));
    }

    #[test]
    fn a_debug_choice_for_dev_is_never_overridden() {
        for text in [
            "[profile.dev]\ndebug = 2\n",
            "[profile.dev]\n  debug=false # mine\n",
            "profile.dev.debug = 0\n[workspace]\n",
            "[profile.dev.package.\"*\"]\nopt-level = 2\n",
        ] {
            assert_eq!(with_dev_profile(text), None, "{text}");
        }
    }

    #[test]
    fn debug_in_other_profiles_does_not_count() {
        let text = "[profile.release]\ndebug = true\n[profile.dev.package.foo]\ndebug = 1\n";
        assert!(with_dev_profile(text).is_some());
    }

    #[test]
    fn advice_writes_nothing_until_asked() {
        let root = scratch("dev-profile");
        let manifest = root.join("Cargo.toml");
        let text = "[workspace]\nmembers = []\n";
        std::fs::write(&manifest, text).unwrap();
        let advice = dev_profile(&root, false).unwrap().unwrap();
        assert!(advice.starts_with("advice   Cargo.toml") && advice.contains("--dev-profile"));
        assert_eq!(std::fs::read_to_string(&manifest).unwrap(), text);
        let wrote = dev_profile(&root, true).unwrap().unwrap();
        assert!(wrote.starts_with("wrote    "));
        let edited = std::fs::read_to_string(&manifest).unwrap();
        assert!(edited.contains("debug = \"line-tables-only\""));
        assert_eq!(dev_profile(&root, true).unwrap(), None);
    }
}
