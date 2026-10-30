//! The `[ci]` table of `soothfast.toml`: where the reusable workflow runs
//! and whether it runs the repo's tests.

use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

use super::ecosystem::table_name;

/// Each field is set or left to its default.
#[derive(Default, Debug, PartialEq, Clone)]
pub struct Ci {
    pub runs_on: Option<Vec<String>>,
    pub test: Option<bool>,
}

impl Ci {
    pub fn parse(text: &str) -> Result<Ci, String> {
        let mut ci = Ci::default();
        let mut in_ci = false;
        for (lineno, line) in logical_lines(text) {
            if let Some(name) = table_name(&line) {
                in_ci = name == "ci";
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
                ("runs_on", TomlValue::Str(label)) => ci.runs_on = Some(vec![label]),
                ("runs_on", TomlValue::StrArray(labels)) if !labels.is_empty() => {
                    ci.runs_on = Some(labels)
                }
                ("runs_on", _) => {
                    return Err(format!(
                        "line {lineno}: `runs_on` is a label or a non-empty list of labels"
                    ));
                }
                ("test", TomlValue::Bool(test)) => ci.test = Some(test),
                ("test", _) => return Err(format!("line {lineno}: `test` is true or false")),
                (k, _) => return Err(format!("line {lineno}: unknown `{k}` under [ci]")),
            }
        }
        Ok(ci)
    }

    /// These settings, with `base` filling what they leave unset.
    pub fn over(self, base: Ci) -> Ci {
        Ci {
            runs_on: self.runs_on.or(base.runs_on),
            test: self.test.or(base.test),
        }
    }

    pub fn is_default(&self) -> bool {
        *self == Ci::default()
    }

    pub fn runs_test(&self) -> bool {
        self.test.unwrap_or(true)
    }

    /// The labels as a JSON string or array, for `fromJSON` in a `runs-on`.
    pub fn runs_on_json(&self) -> String {
        let labels = self
            .runs_on
            .clone()
            .unwrap_or_else(|| vec!["ubuntu-latest".into()]);
        match labels.as_slice() {
            [one] => serde_json::json!(one).to_string(),
            many => serde_json::json!(many).to_string(),
        }
    }

    /// The `[ci]` table, with only the fields that are set.
    pub fn section(&self) -> String {
        let mut text = String::from("[ci]\n");
        if let Some(labels) = &self.runs_on {
            text.push_str(&format!("runs_on = {}\n", serde_json::json!(labels)));
        }
        if let Some(test) = self.test {
            text.push_str(&format!("test = {test}\n"));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_or_a_list_names_the_runners() {
        let one = Ci::parse("[ci]\nruns_on = \"self-hosted\"\n").unwrap();
        assert_eq!(one.runs_on_json(), "\"self-hosted\"");
        let many = Ci::parse("[ci]\nruns_on = [\"self-hosted\", \"linux\"]\n").unwrap();
        assert_eq!(many.runs_on_json(), "[\"self-hosted\",\"linux\"]");
        assert_eq!(Ci::default().runs_on_json(), "\"ubuntu-latest\"");
    }

    #[test]
    fn tests_run_unless_switched_off() {
        assert!(Ci::default().runs_test());
        assert!(!Ci::parse("[ci]\ntest = false\n").unwrap().runs_test());
    }

    #[test]
    fn the_section_round_trips_and_skips_other_tables() {
        let ci = Ci {
            runs_on: Some(vec!["a".into(), "b".into()]),
            test: Some(false),
        };
        let text = format!("[gate]\nwalltime = \"hard\"\n\n{}", ci.section());
        assert_eq!(Ci::parse(&text), Ok(ci));
        assert_eq!(Ci::parse("[gate]\nx = 1\n"), Ok(Ci::default()));
    }

    #[test]
    fn a_bad_ci_table_is_an_error() {
        assert!(Ci::parse("[ci]\nruns_on = []\n").is_err());
        assert!(Ci::parse("[ci]\ntest = \"no\"\n").is_err());
        assert!(Ci::parse("[ci]\nnope = 1\n").is_err());
    }

    #[test]
    fn flags_override_the_table() {
        let flags = Ci {
            test: Some(false),
            ..Ci::default()
        };
        let table = Ci {
            runs_on: Some(vec!["x".into()]),
            test: Some(true),
        };
        let merged = flags.over(table);
        assert_eq!(merged.test, Some(false));
        assert_eq!(merged.runs_on, Some(vec!["x".into()]));
    }
}
