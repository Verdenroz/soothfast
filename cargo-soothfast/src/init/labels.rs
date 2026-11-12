//! Creating the labels the bot acts on with `gh`.

use std::path::Path;
use std::process::Command;

use super::bot::BotLabels;
use super::err;

const MERGE_LABEL: (&str, &str) = ("0e8a16", "Merge once the required checks pass");
const UPDATE_LABEL: (&str, &str) = (
    "1d76db",
    "Keep this branch up to date with the default branch",
);
const IGNORE_LABEL: (&str, &str) = ("b60205", "Cancel any queued merge");
const PERF_ACCEPTED_LABEL: (&str, &str) = ("fbca04", "The gate's regressions are intended");

/// The exit code of label creation: a failure only warns when `--bot`
/// implied the labels rather than `--labels` asking for them.
pub(super) fn label_exit(created: Result<(), String>, implied: bool) -> i32 {
    match created {
        Ok(()) => 0,
        Err(e) if implied => {
            println!("warn     {e}; run `cargo soothfast init --labels` once gh works");
            0
        }
        Err(e) => err(&e),
    }
}

/// Create or refresh the labels on the repository `gh` resolves here, named
/// and styled as `[bot]` configures them.
pub(super) fn create_labels(root: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap_or_default();
    let configured = match BotLabels::parse(&text) {
        Ok(labels) => labels,
        Err(e) => return Err(format!("soothfast.toml: {e}")),
    };
    let set = label_set(&configured).map_err(|e| format!("soothfast.toml: {e}"))?;
    for style in &configured.styles {
        if !set.iter().any(|(name, ..)| *name == style.name) {
            println!(
                "warn     [[bot.label]] names {}, which no role uses",
                style.name
            );
        }
    }
    for (name, color, description) in set {
        let output = Command::new("gh")
            .args(["label", "create", &name, "--force", "--color", &color])
            .args(["--description", &description])
            .output()
            .map_err(|e| {
                format!("could not create label {name}: gh could not run ({e}); is it installed?")
            })?;
        if !output.status.success() {
            return Err(label_failure(&name, &output.stderr));
        }
        println!("label    {name}");
    }
    Ok(())
}

/// The error for a label `gh` refused: its first non-empty line of stderr.
fn label_failure(name: &str, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let reason = stderr
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("is gh signed in?");
    format!("could not create label {name}: {reason}")
}

/// Every label soothfast acts on, with its colour and description: a
/// `[[bot.label]]` entry's where it names the label, else its role's.
fn label_set(labels: &BotLabels) -> Result<Vec<(String, String, String)>, String> {
    let group = |names: &[String], (role_color, role_description): (&str, &str)| {
        names
            .iter()
            .map(|name| {
                let style = labels.styles.iter().find(|s| s.name == *name);
                let color = style.and_then(|s| s.color.as_deref()).unwrap_or(role_color);
                let description = style
                    .and_then(|s| s.description.as_deref())
                    .unwrap_or(role_description);
                if color.len() != 6 || !color.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(format!("{name}: color {color:?} is not 6 hex digits"));
                }
                Ok((name.clone(), color.to_string(), description.to_string()))
            })
            .collect::<Result<Vec<_>, String>>()
    };
    Ok([
        group(&labels.merge, MERGE_LABEL)?,
        group(&labels.update, UPDATE_LABEL)?,
        group(&labels.perf_accepted, PERF_ACCEPTED_LABEL)?,
        group(&labels.ignore, IGNORE_LABEL)?,
    ]
    .concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implied_label_failures_warn_and_explicit_ones_fail() {
        let failed = || Err("could not create label x".to_string());
        assert_eq!(label_exit(failed(), true), 0);
        assert_eq!(label_exit(failed(), false), 2);
        assert_eq!(label_exit(Ok(()), false), 0);
    }

    #[test]
    fn a_gh_failure_keeps_only_its_first_line() {
        let stderr = b"no remotes found\nrun gh repo set-default\n";
        assert_eq!(
            label_failure("x", stderr),
            "could not create label x: no remotes found"
        );
        assert_eq!(
            label_failure("x", b"\n  \nlater\n"),
            "could not create label x: later"
        );
        assert_eq!(
            label_failure("x", b""),
            "could not create label x: is gh signed in?"
        );
    }

    #[test]
    fn the_label_set_follows_the_configured_names() {
        let configured = BotLabels::parse("[bot]\nmerge_labels = [\"ship\"]\n").unwrap();
        let names: Vec<String> = label_set(&configured)
            .unwrap()
            .into_iter()
            .map(|(n, ..)| n)
            .collect();
        assert_eq!(
            names,
            [
                "ship",
                "soothfast:update",
                "soothfast:perf-accepted",
                "do-not-merge"
            ]
        );
    }

    #[test]
    fn a_label_entry_styles_the_label_it_names() {
        let text = "[bot]\nmerge_labels = [\"ship\"]\n\n[[bot.label]]\nname = \"ship\"\n\
                    color = \"123abc\"\ndescription = \"Go\"\n";
        let set = label_set(&BotLabels::parse(text).unwrap()).unwrap();
        assert_eq!(set[0], ("ship".into(), "123abc".into(), "Go".into()));
        assert_eq!(set[1].1, "1d76db");
    }

    #[test]
    fn a_bad_label_colour_is_an_error() {
        let text = "[[bot.label]]\nname = \"do-not-merge\"\ncolor = \"#b60205\"\n";
        let err = label_set(&BotLabels::parse(text).unwrap()).unwrap_err();
        assert!(err.contains("do-not-merge") && err.contains("6 hex digits"));
    }
}
