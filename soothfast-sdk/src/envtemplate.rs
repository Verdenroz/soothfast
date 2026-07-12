//! Reading a server's dotenv template as its configuration surface.
//!
//! A server that documents its knobs in a `.env.template` has already
//! written the thing an SDK needs: the names, the defaults, and the prose
//! explaining each one. Parsing that file is what lets an embedded server
//! be configured *the same way* whether it is deployed or spawned by a
//! `pip install`ed client — one source, two consumers, no second list to
//! keep in step.
//!
//! The grammar is the dotenv subset every such file already uses:
//! `KEY=value` lines, `#` comments above an entry as its documentation,
//! and `# KEY=value` for a knob deliberately left unset.

/// One environment variable the embedded server reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    /// Comment lines attached to the entry, in template order.
    pub doc: Vec<String>,
    /// The value the template ships. `None` when the entry is commented
    /// out — the knob exists, the server runs without it.
    pub default: Option<String>,
}

impl EnvVar {
    /// Whether the template leaves this one unset.
    pub fn optional(&self) -> bool {
        self.default.is_none()
    }
}

/// Parse a dotenv-style template into the knobs it documents.
///
/// Anything unparseable is skipped rather than rejected: the file belongs
/// to the server, and an SDK is not the place a stray line becomes fatal.
///
/// Names are unique: a template that shows `# KEY=example` above the real
/// `KEY=` names one knob, and emitting it twice would not compile.
pub fn parse(text: &str) -> Vec<EnvVar> {
    let mut vars: Vec<EnvVar> = Vec::new();
    let mut doc: Vec<String> = Vec::new();

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            // A blank line ends a comment block: prose two paragraphs up
            // is not documentation for what follows.
            doc.clear();
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            let comment = comment.trim();
            // `# KEY=value` is a knob left unset, not prose about one.
            match assignment(comment) {
                Some((name, _)) => record(
                    &mut vars,
                    EnvVar {
                        name,
                        doc: std::mem::take(&mut doc),
                        default: None,
                    },
                ),
                None if !comment.is_empty() => doc.push(comment.to_string()),
                None => {}
            }
            continue;
        }
        if let Some((name, value)) = assignment(line) {
            let (value, trailing) = split_inline_comment(&value);
            if let Some(note) = trailing {
                doc.push(note);
            }
            record(
                &mut vars,
                EnvVar {
                    name,
                    doc: std::mem::take(&mut doc),
                    default: Some(value),
                },
            );
        }
    }
    vars
}

/// Add a knob, or let a repeated name update the one already recorded —
/// keeping its position, and any documentation the later line omitted.
fn record(vars: &mut Vec<EnvVar>, var: EnvVar) {
    match vars.iter_mut().find(|v| v.name == var.name) {
        Some(existing) => {
            if !var.doc.is_empty() {
                existing.doc = var.doc;
            }
            if var.default.is_some() {
                existing.default = var.default;
            }
        }
        None => vars.push(var),
    }
}

/// Render the knobs as a README table. Same text in both languages: the
/// variables belong to the server, not to whoever is calling it.
pub fn markdown_table(vars: &[EnvVar]) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    let _ = writeln!(out, "| Variable | Default | Notes |");
    let _ = writeln!(out, "| --- | --- | --- |");
    for var in vars {
        let default = match var.default.as_deref() {
            Some("") | None => "_unset_".to_string(),
            Some(value) => format!("`{value}`"),
        };
        let notes = var
            .doc
            .iter()
            .map(|line| escape_cell(line))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = writeln!(out, "| `{}` | {default} | {notes} |", var.name);
    }
    out
}

/// A `|` inside a cell would end it early.
fn escape_cell(text: &str) -> String {
    text.replace('|', "\\|")
}

/// Split `KEY=value` when `KEY` looks like an environment variable —
/// uppercase, digits and underscores, not starting with a digit.
fn assignment(line: &str) -> Option<(String, String)> {
    let (name, value) = line.split_once('=')?;
    let name = name.trim_start_matches("export ").trim();
    let ok = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    ok.then(|| (name.to_string(), value.trim().to_string()))
}

/// Peel a trailing `# …` note off an unquoted value, the way every dotenv
/// reader does. A quoted value keeps whatever is inside the quotes.
fn split_inline_comment(value: &str) -> (String, Option<String>) {
    if let Some(inner) = unquote(value) {
        return (inner, None);
    }
    match value.split_once('#') {
        Some((value, note)) => {
            let note = note.trim();
            (
                value.trim().to_string(),
                (!note.is_empty()).then(|| note.to_string()),
            )
        }
        None => (value.to_string(), None),
    }
}

fn unquote(value: &str) -> Option<String> {
    for quote in ['"', '\''] {
        if let Some(rest) = value.strip_prefix(quote)
            && let Some(inner) = rest.strip_suffix(quote)
        {
            return Some(inner.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = "\
# Server Configuration
PORT=8000

# Logging
# Options: DEBUG, INFO, WARNING, ERROR
LOG_LEVEL=INFO

# API Defaults
DEFAULT_INTERVAL=1d    # Options: 1m, 5m, 1d

# FRED API Integration (optional)
# Free API key from https://fred.stlouisfed.org/
# FRED_API_KEY=your_fred_api_key_here
";

    #[test]
    fn comments_above_an_entry_become_its_documentation() {
        let vars = parse(TEMPLATE);
        let port = &vars[0];
        assert_eq!(port.name, "PORT");
        assert_eq!(port.doc, ["Server Configuration"]);
        assert_eq!(port.default.as_deref(), Some("8000"));

        let level = &vars[1];
        assert_eq!(
            level.doc,
            ["Logging", "Options: DEBUG, INFO, WARNING, ERROR"]
        );
    }

    #[test]
    fn a_commented_out_entry_is_a_knob_with_no_default() {
        let vars = parse(TEMPLATE);
        let fred = vars
            .iter()
            .find(|v| v.name == "FRED_API_KEY")
            .expect("found");
        assert!(fred.optional(), "commented out means unset by default");
        assert_eq!(
            fred.doc,
            [
                "FRED API Integration (optional)",
                "Free API key from https://fred.stlouisfed.org/"
            ]
        );
    }

    #[test]
    fn an_inline_note_documents_rather_than_corrupts_the_default() {
        let vars = parse(TEMPLATE);
        let interval = vars
            .iter()
            .find(|v| v.name == "DEFAULT_INTERVAL")
            .expect("found");
        assert_eq!(interval.default.as_deref(), Some("1d"));
        assert_eq!(interval.doc, ["API Defaults", "Options: 1m, 5m, 1d"]);
    }

    #[test]
    fn a_blank_line_ends_a_comment_block() {
        let vars = parse("# a heading\n\nKEY=value\n");
        assert_eq!(vars[0].doc, Vec::<String>::new());
    }

    #[test]
    fn prose_that_merely_contains_an_equals_sign_is_not_an_entry() {
        let vars = parse("# note: set this when x = y\nKEY=value\n");
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].name, "KEY");
        assert_eq!(vars[0].doc, ["note: set this when x = y"]);
    }

    #[test]
    fn an_example_above_the_real_entry_names_one_knob() {
        let vars = parse("# DATABASE_URL=postgres://user@host/db\nDATABASE_URL=\n");
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].name, "DATABASE_URL");
        assert_eq!(vars[0].default.as_deref(), Some(""));
    }

    #[test]
    fn a_repeated_entry_keeps_its_first_position() {
        let vars = parse("A=1\nB=2\nA=3\n");
        assert_eq!(
            vars.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
            ["A", "B"]
        );
        assert_eq!(vars[0].default.as_deref(), Some("3"));
    }

    #[test]
    fn a_quoted_value_keeps_its_hashes() {
        let vars = parse("KEY=\"a # b\"\n");
        assert_eq!(vars[0].default.as_deref(), Some("a # b"));
    }
}
