//! `cargo soothfast labels`: the label names `[bot]` configures, for the
//! action's shell scripts.

use std::path::Path;

use crate::init::bot::BotLabels;
use crate::invoke;

const ROLES: [&str; 4] = ["merge", "update", "ignore", "perf-accepted"];

pub fn run(args: &[String]) -> i32 {
    let (mut git_ref, mut role) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let slot = match a.as_str() {
            "--ref" => &mut git_ref,
            "--role" => &mut role,
            other => return crate::arg_err(&format!("unknown labels arg {other:?}")),
        };
        match it.next() {
            Some(v) => *slot = Some(v.clone()),
            None => return crate::arg_err(&format!("{a} needs a value")),
        }
    }
    let printed = config(&invoke::project_root(), git_ref.as_deref())
        .and_then(|text| render(&text, role.as_deref()));
    match printed {
        Ok(out) => {
            print!("{out}");
            0
        }
        Err(e) => {
            eprintln!("soothfast: labels: {e}");
            2
        }
    }
}

/// The text of `soothfast.toml`: as committed at `git_ref` when given (the
/// file must exist there), else from the working tree, empty when absent.
fn config(root: &Path, git_ref: Option<&str>) -> Result<String, String> {
    match git_ref {
        Some(r) => invoke::git_in(root, &["show", &format!("{r}:./soothfast.toml")])
            .map_err(|e| e.to_string()),
        None => Ok(std::fs::read_to_string(root.join("soothfast.toml")).unwrap_or_default()),
    }
}

fn render(text: &str, role: Option<&str>) -> Result<String, String> {
    let labels = BotLabels::parse(text).map_err(|e| format!("soothfast.toml: {e}"))?;
    let mut out = String::new();
    match role {
        Some(role) => {
            let names = labels.role(role).ok_or(format!(
                "unknown role {role:?}; one of {}",
                ROLES.join(", ")
            ))?;
            for name in names {
                out.push_str(&format!("{name}\n"));
            }
        }
        None => {
            for role in ROLES {
                for name in labels.role(role).unwrap_or_default() {
                    out.push_str(&format!("{role}\t{name}\n"));
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use super::*;

    fn repo(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("soothfast-labels-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        root
    }

    fn git(root: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }

    #[test]
    fn every_role_prints_with_its_name() {
        let out = render("[bot]\nmerge_labels = [\"ship\", \"land\"]\n", None).unwrap();
        assert_eq!(
            out,
            "merge\tship\nmerge\tland\nupdate\tsoothfast:update\nignore\tdo-not-merge\n\
             perf-accepted\tsoothfast:perf-accepted\n"
        );
    }

    #[test]
    fn a_role_prints_one_name_per_line() {
        let text = "[bot]\nperf_accepted_labels = [\"a\", \"b\"]\n";
        assert_eq!(render(text, Some("perf-accepted")).unwrap(), "a\nb\n");
        assert!(
            render("", Some("nope"))
                .unwrap_err()
                .contains("unknown role")
        );
    }

    #[test]
    fn a_malformed_table_is_an_error() {
        let err = render("[bot]\nperf_accepted_labels = \"x\"\n", None).unwrap_err();
        assert!(err.contains("perf_accepted_labels"));
    }

    #[test]
    fn a_ref_reads_the_committed_file_not_the_working_tree() {
        let root = repo("ref");
        let file = root.join("soothfast.toml");
        std::fs::write(&file, "[bot]\nperf_accepted_labels = [\"waive\"]\n").unwrap();
        git(&root, &["add", "soothfast.toml"]);
        git(&root, &["commit", "-qm", "x"]);
        std::fs::write(&file, "[bot]\nperf_accepted_labels = [\"enhancement\"]\n").unwrap();
        let text = config(&root, Some("HEAD")).unwrap();
        assert_eq!(render(&text, Some("perf-accepted")).unwrap(), "waive\n");
        assert!(config(&root, None).unwrap().contains("enhancement"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_ref_reads_the_file_beside_the_project_root() {
        let root = repo("subdir");
        let project = root.join("rust");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("soothfast.toml"),
            "[bot]\nperf_accepted_labels = [\"waive\"]\n",
        )
        .unwrap();
        git(&root, &["add", "rust"]);
        git(&root, &["commit", "-qm", "x"]);
        assert!(config(&root, Some("HEAD")).is_err());
        let text = config(&project, Some("HEAD")).unwrap();
        assert_eq!(render(&text, Some("perf-accepted")).unwrap(), "waive\n");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_ref_without_the_file_is_an_error() {
        let root = repo("missing");
        std::fs::write(root.join("other"), "x").unwrap();
        git(&root, &["add", "other"]);
        git(&root, &["commit", "-qm", "x"]);
        assert!(config(&root, Some("HEAD")).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
