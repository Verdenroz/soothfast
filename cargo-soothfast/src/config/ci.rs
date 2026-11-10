//! The `[ci]` table of `soothfast.toml`.
//!
//! `soothfast.toml` is shared with the site, spec and gate engines, so this
//! parser skips every table it doesn't own.

use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

/// Parse `skip` from the `[ci]` table of a `soothfast.toml`: the CI job
/// kinds not to run. The names are free-form.
pub fn parse(text: &str) -> Result<Vec<String>, String> {
    let mut skip = Vec::new();
    let mut in_ci = false;

    for (lineno, line) in logical_lines(text) {
        let line = line.as_str();
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_ci = inner.trim() == "ci";
            continue;
        }
        if !in_ci {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("line {lineno}: expected `key = value`"));
        };
        let value = parse_value(value.trim()).map_err(|e| format!("line {lineno}: {e}"))?;
        match (key.trim(), value) {
            ("skip", TomlValue::StrArray(kinds)) => skip = kinds,
            (k, _) => {
                return Err(format!(
                    "line {lineno}: unknown or mistyped `{k}` under [ci]"
                ));
            }
        }
    }
    Ok(skip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_lists_job_kinds_and_other_tables_are_ignored() {
        let text = "[docs]\npaths = [\"x\"]\n\n[ci]\nskip = [\"sdk\", \"anything\"]\n";
        assert_eq!(parse(text).unwrap(), ["sdk", "anything"]);
        assert!(parse("[gate]\nfeatures = \"a\"\n").unwrap().is_empty());
    }

    #[test]
    fn a_mistyped_or_unknown_key_names_its_line() {
        let e = parse("[ci]\nskip = \"sdk\"\n").unwrap_err();
        assert_eq!(e, "line 2: unknown or mistyped `skip` under [ci]");
        assert!(parse("[ci]\njobs = [\"a\"]\n").is_err());
    }
}
