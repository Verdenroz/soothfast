//! Where the reusable workflow runs and whether it runs the repo's tests:
//! `[runner] labels` and `[test] ci` in `soothfast.toml`.

use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

use super::ecosystem::table_name;

/// Each field is set or left to its default.
#[derive(Default, Debug, PartialEq, Clone)]
pub struct Ci {
    pub labels: Option<Vec<String>>,
    pub test: Option<bool>,
}

impl Ci {
    /// The `[runner]` table and the `ci` key of `[test]`; other tables and
    /// the rest of `[test]` belong to other readers.
    pub fn parse(text: &str) -> Result<Ci, String> {
        let mut ci = Ci::default();
        let mut table = "";
        for (lineno, line) in logical_lines(text) {
            if let Some(name) = table_name(&line) {
                table = match name {
                    "runner" => "runner",
                    "test" => "test",
                    _ => "",
                };
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                if table.is_empty() {
                    continue;
                }
                return Err(format!("line {lineno}: expected `key = value`"));
            };
            let key = key.trim();
            match (table, key) {
                ("runner", "labels") => match parse_value(value.trim()) {
                    Ok(TomlValue::StrArray(labels)) if !labels.is_empty() => {
                        ci.labels = Some(labels)
                    }
                    _ => {
                        return Err(format!(
                            "line {lineno}: `labels` under [runner] is a non-empty list of strings"
                        ));
                    }
                },
                ("runner", k) => {
                    return Err(format!("line {lineno}: unknown `{k}` under [runner]"));
                }
                ("test", "ci") => match parse_value(value.trim()) {
                    Ok(TomlValue::Bool(test)) => ci.test = Some(test),
                    _ => return Err(format!("line {lineno}: `ci` under [test] is true or false")),
                },
                _ => {}
            }
        }
        Ok(ci)
    }

    /// These settings, with `base` filling what they leave unset.
    pub fn over(self, base: Ci) -> Ci {
        Ci {
            labels: self.labels.or(base.labels),
            test: self.test.or(base.test),
        }
    }

    /// Whether the settings leave the workflow at its defaults.
    pub fn is_default(&self) -> bool {
        self.labels.is_none() && self.runs_test()
    }

    pub fn runs_test(&self) -> bool {
        self.test.unwrap_or(true)
    }

    /// The labels as a JSON string or array, for `fromJSON` in a `runs-on`.
    pub fn runs_on_json(&self) -> String {
        let labels = self
            .labels
            .clone()
            .unwrap_or_else(|| vec!["ubuntu-latest".into()]);
        match labels.as_slice() {
            [one] => serde_json::json!(one).to_string(),
            many => serde_json::json!(many).to_string(),
        }
    }

    /// The `[runner]` table.
    pub fn runner_section(&self) -> String {
        let labels = self.labels.clone().unwrap_or_default();
        format!("[runner]\nlabels = {}\n", serde_json::json!(labels))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_labels_become_a_json_runs_on() {
        let one = Ci::parse("[runner]\nlabels = [\"self-hosted\"]\n").unwrap();
        assert_eq!(one.runs_on_json(), "\"self-hosted\"");
        let many = Ci::parse("[runner]\nlabels = [\"self-hosted\", \"linux\"]\n").unwrap();
        assert_eq!(many.runs_on_json(), "[\"self-hosted\",\"linux\"]");
        assert_eq!(Ci::default().runs_on_json(), "\"ubuntu-latest\"");
    }

    #[test]
    fn tests_run_unless_the_test_table_turns_ci_off() {
        assert!(Ci::default().runs_test());
        let off = "[test]\nignore = [\"docs/**\"]\nci = false\n";
        assert!(!Ci::parse(off).unwrap().runs_test());
        assert!(
            Ci::parse("[test]\nignore = [\"docs/**\"]\n")
                .unwrap()
                .is_default()
        );
    }

    #[test]
    fn the_runner_section_round_trips_and_other_tables_are_skipped() {
        let ci = Ci {
            labels: Some(vec!["a".into(), "b".into()]),
            test: None,
        };
        let text = format!("[gate]\nwalltime = \"hard\"\n\n{}", ci.runner_section());
        assert_eq!(Ci::parse(&text), Ok(ci));
        assert_eq!(Ci::parse("[gate]\nx = 1\n"), Ok(Ci::default()));
    }

    #[test]
    fn a_bad_table_is_an_error() {
        assert!(Ci::parse("[runner]\nlabels = []\n").is_err());
        assert!(Ci::parse("[runner]\nlabels = \"x\"\n").is_err());
        assert!(Ci::parse("[runner]\nnope = 1\n").is_err());
        assert!(Ci::parse("[test]\nci = \"no\"\n").is_err());
    }

    #[test]
    fn flags_override_the_table() {
        let flags = Ci {
            test: Some(false),
            ..Ci::default()
        };
        let table = Ci {
            labels: Some(vec!["x".into()]),
            test: Some(true),
        };
        let merged = flags.over(table);
        assert_eq!(merged.test, Some(false));
        assert_eq!(merged.labels, Some(vec!["x".into()]));
    }
}
