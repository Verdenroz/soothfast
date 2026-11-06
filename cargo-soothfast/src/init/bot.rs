//! The label names the `[bot]` table of `soothfast.toml` configures. The
//! broker reads the rest of the table; `init --labels` needs only these.

use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

use super::ecosystem::table_name;

/// The label names soothfast-bot acts on.
#[derive(Debug, PartialEq)]
pub struct BotLabels {
    pub merge: Vec<String>,
    pub update: Vec<String>,
    pub ignore: Vec<String>,
}

impl Default for BotLabels {
    fn default() -> Self {
        let names = |name: &str| vec![name.to_string()];
        BotLabels {
            merge: names("soothfast:merge"),
            update: names("soothfast:update"),
            ignore: names("do-not-merge"),
        }
    }
}

impl BotLabels {
    /// The `*_labels` keys of `[bot]` over the defaults.
    pub fn parse(text: &str) -> Result<BotLabels, String> {
        let mut labels = BotLabels::default();
        let mut in_bot = false;
        for (lineno, line) in logical_lines(text) {
            if let Some(name) = table_name(&line) {
                in_bot = name == "bot";
                continue;
            }
            let Some((key, value)) = line.split_once('=').filter(|_| in_bot) else {
                continue;
            };
            let key = key.trim();
            let slot = match key {
                "merge_labels" => &mut labels.merge,
                "update_labels" => &mut labels.update,
                "ignore_labels" => &mut labels.ignore,
                _ => continue,
            };
            match parse_value(value.trim()) {
                Ok(TomlValue::StrArray(names)) => *slot = names,
                _ => {
                    return Err(format!(
                        "line {lineno}: `{key}` under [bot] is a list of strings"
                    ));
                }
            }
        }
        Ok(labels)
    }
}

/// Whether the text has a `[bot]` table.
pub fn has_bot_table(text: &str) -> bool {
    text.lines().any(|l| table_name(l) == Some("bot"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_apply_without_a_bot_table() {
        let labels = BotLabels::parse("[gate]\nwalltime = \"advisory\"\n").unwrap();
        assert_eq!(labels, BotLabels::default());
    }

    #[test]
    fn configured_names_replace_the_defaults() {
        let text =
            "[bot]\nlabels = true\nmerge_labels = [\"ship\", \"land\"]\nignore_labels = []\n";
        let labels = BotLabels::parse(text).unwrap();
        assert_eq!(labels.merge, ["ship", "land"]);
        assert_eq!(labels.update, ["soothfast:update"]);
        assert!(labels.ignore.is_empty());
    }

    #[test]
    fn keys_of_other_tables_are_not_read() {
        let labels = BotLabels::parse("[other]\nmerge_labels = [\"x\"]\n").unwrap();
        assert_eq!(labels, BotLabels::default());
    }

    #[test]
    fn a_label_list_that_is_not_strings_is_an_error() {
        let err = BotLabels::parse("[bot]\nmerge_labels = \"ship\"\n").unwrap_err();
        assert!(err.contains("merge_labels"));
    }

    #[test]
    fn the_bot_table_is_found_by_its_header() {
        assert!(has_bot_table("[gate]\n\n[bot]\nlabels = true\n"));
        assert!(!has_bot_table("[bot.extra]\n# [bot]\n"));
    }
}
