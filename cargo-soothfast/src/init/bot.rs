//! The label names and looks the `[bot]` table of `soothfast.toml`
//! configures. The broker reads the rest of the table; `init --labels` and
//! `cargo soothfast labels` need only these.

use std::collections::BTreeMap;

use soothfast_site::toml::{TomlValue, logical_lines, parse_value};

use super::ecosystem::table_name;

/// The label names soothfast-bot acts on.
#[derive(Debug, PartialEq)]
pub struct BotLabels {
    pub merge: Vec<String>,
    pub update: Vec<String>,
    pub ignore: Vec<String>,
    pub perf_accepted: Vec<String>,
    pub styles: Vec<LabelStyle>,
}

/// A `[[bot.label]]` entry: the colour and description `init` creates the
/// label named `name` with.
#[derive(Debug, PartialEq)]
pub struct LabelStyle {
    pub name: String,
    pub color: Option<String>,
    pub description: Option<String>,
}

impl Default for BotLabels {
    fn default() -> Self {
        let names = |name: &str| vec![name.to_string()];
        BotLabels {
            merge: names("soothfast:merge"),
            update: names("soothfast:update"),
            ignore: names("do-not-merge"),
            perf_accepted: names("soothfast:perf-accepted"),
            styles: Vec::new(),
        }
    }
}

impl BotLabels {
    /// The `*_labels` keys of `[bot]` and the `[[bot.label]]` entries over the
    /// defaults.
    pub fn parse(text: &str) -> Result<BotLabels, String> {
        let mut labels = BotLabels::default();
        for (lineno, line) in bot_keys(text) {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let slot = match key {
                "merge_labels" => &mut labels.merge,
                "update_labels" => &mut labels.update,
                "ignore_labels" => &mut labels.ignore,
                "perf_accepted_labels" => &mut labels.perf_accepted,
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
        labels.styles = label_styles(text)?;
        Ok(labels)
    }

    /// The names configured for a role, spelled as `cargo soothfast labels
    /// --role` takes it.
    pub fn role(&self, role: &str) -> Option<&[String]> {
        match role {
            "merge" => Some(&self.merge),
            "update" => Some(&self.update),
            "ignore" => Some(&self.ignore),
            "perf-accepted" => Some(&self.perf_accepted),
            _ => None,
        }
    }
}

/// The `[[bot.label]]` entries of the text.
fn label_styles(text: &str) -> Result<Vec<LabelStyle>, String> {
    let mut styles = Vec::new();
    let mut open: Option<(usize, BTreeMap<String, String>)> = None;
    let mut finish = |open: Option<(usize, BTreeMap<String, String>)>| {
        let Some((lineno, mut fields)) = open else {
            return Ok(());
        };
        let name = fields
            .remove("name")
            .ok_or(format!("line {lineno}: [[bot.label]] needs a `name`"))?;
        styles.push(LabelStyle {
            name,
            color: fields.remove("color"),
            description: fields.remove("description"),
        });
        Ok::<(), String>(())
    };
    for (lineno, line) in logical_lines(text) {
        if let Some(table) = table_name(&line) {
            finish(open.take())?;
            open = (table == "[bot.label]").then(|| (lineno, BTreeMap::new()));
            continue;
        }
        let Some((_, fields)) = open.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !matches!(key, "name" | "color" | "description") {
            continue;
        }
        let Ok(TomlValue::Str(value)) = parse_value(value.trim()) else {
            return Err(format!(
                "line {lineno}: `{key}` under [[bot.label]] is a string"
            ));
        };
        fields.insert(key.to_string(), value);
    }
    finish(open)?;
    Ok(styles)
}

/// Whether `[bot]` turns the bot on with `labels = true`.
pub fn bot_enabled(text: &str) -> bool {
    bot_keys(text)
        .into_iter()
        .filter_map(|(_, line)| {
            line.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .find(|(key, _)| key == "labels")
        .is_some_and(|(_, value)| parse_value(&value) == Ok(TomlValue::Bool(true)))
}

/// Whether the text has a `[bot]` table.
pub fn has_bot_table(text: &str) -> bool {
    logical_lines(text)
        .iter()
        .any(|(_, line)| table_name(line) == Some("bot"))
}

/// The logical lines under the `[bot]` header, with their line numbers.
fn bot_keys(text: &str) -> Vec<(usize, String)> {
    let mut in_bot = false;
    let mut keys = Vec::new();
    for (lineno, line) in logical_lines(text) {
        match table_name(&line) {
            Some(name) => in_bot = name == "bot",
            None if in_bot => keys.push((lineno, line)),
            None => {}
        }
    }
    keys
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
    fn the_waiver_names_and_label_styles_are_read() {
        let text = "[bot]\nperf_accepted_labels = [\"waive\", \"ok\"]\n\n\
                    [[bot.label]]\nname = \"waive\"\ncolor = \"fbca04\"\n\n\
                    [[bot.label]]\nname = \"ship\"\ndescription = \"Go\"\n";
        let labels = BotLabels::parse(text).unwrap();
        assert_eq!(labels.perf_accepted, ["waive", "ok"]);
        assert_eq!(
            labels.styles,
            [
                LabelStyle {
                    name: "waive".into(),
                    color: Some("fbca04".into()),
                    description: None
                },
                LabelStyle {
                    name: "ship".into(),
                    color: None,
                    description: Some("Go".into())
                },
            ]
        );
    }

    #[test]
    fn a_label_entry_without_a_name_is_an_error() {
        let err = BotLabels::parse("[[bot.label]]\ncolor = \"fbca04\"\n").unwrap_err();
        assert!(err.contains("needs a `name`"));
    }

    #[test]
    fn label_entries_do_not_leak_into_the_bot_table() {
        let text = "[[bot.label]]\nname = \"x\"\nmerge_labels = [\"y\"]\n";
        assert_eq!(BotLabels::parse(text).unwrap().merge, ["soothfast:merge"]);
    }

    #[test]
    fn the_bot_is_enabled_only_by_labels_true_under_bot() {
        assert!(bot_enabled("[bot]\nlabels = true\n"));
        assert!(!bot_enabled("[bot]\nmerge_labels = [\"ship\"]\n"));
        assert!(!bot_enabled("[bot]\nlabels = false\n"));
        assert!(!bot_enabled("[other]\nlabels = true\n"));
        assert!(!bot_enabled(""));
    }

    #[test]
    fn the_bot_table_is_found_by_its_header() {
        assert!(has_bot_table("[gate]\n\n[bot]\nlabels = true\n"));
        assert!(!has_bot_table("[bot.extra]\n# [bot]\n"));
    }
}
