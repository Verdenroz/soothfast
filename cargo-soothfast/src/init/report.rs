//! What `init` prints for each scaffolded file, and the drift `--check`
//! finds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::bot::has_bot_table;
use super::ci::Ci;
use super::ecosystem::Settings;
use super::scaffold::{Kind, Scaffold, State, path_diff, read_text, scaffold};
use super::target::target;
use super::workflow::WORKFLOW_PATH;
use super::{Pins, err, other_callers};

fn label(f: &Scaffold) -> String {
    match f.kind {
        Kind::Section => format!("{} [[project]]", f.path.display()),
        Kind::Runner => format!("{} [runner]", f.path.display()),
        Kind::Bot => format!("{} [bot]", f.path.display()),
        Kind::TestCi => format!("{} [test] ci", f.path.display()),
        Kind::Docs => format!("{} [docs] package", f.path.display()),
        _ => f.path.display().to_string(),
    }
}

pub(super) fn skips_workflow(
    root: &Path,
    callers: &[String],
    duplicate_only: bool,
    force: bool,
) -> bool {
    !callers.is_empty() && duplicate_only && !force && !root.join(WORKFLOW_PATH).exists()
}

/// The files a flagless `init --check` reports, by path.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Drift {
    pub(crate) missing: Vec<String>,
    pub(crate) differs: Vec<String>,
}

/// What `init --check` finds, computed without the network and without
/// printing or writing.
pub(crate) fn drift(root: &Path) -> Result<Drift, String> {
    let (mut ws, _) = target(
        root,
        Settings::default(),
        None,
        Ci::default(),
        Pins::default(),
        &[],
        true,
        true,
        true,
    )?;
    ws.labels = true;
    let duplicate_only = ws.projects.is_empty() && ws.ci.is_default();
    let skip = skips_workflow(root, &other_callers(root), duplicate_only, false);
    let mut out = Drift::default();
    for f in scaffold(&ws)
        .iter()
        .filter(|f| !skip || f.kind == Kind::Bot)
    {
        let bucket = match f.state(read_text(root, &f.path)?.as_deref()) {
            State::Current => continue,
            State::Missing => &mut out.missing,
            State::Differs => &mut out.differs,
        };
        let path = f.path.display().to_string();
        if !bucket.contains(&path) {
            bucket.push(path);
        }
    }
    Ok(out)
}

pub(super) fn report(root: &Path, files: &[Scaffold], check: bool, force: bool) -> i32 {
    let mut texts: BTreeMap<&Path, Result<Option<String>, String>> = BTreeMap::new();
    for f in files {
        texts
            .entry(&f.path)
            .or_insert_with(|| read_text(root, &f.path));
    }
    let current = |texts: &BTreeMap<&Path, Result<Option<String>, String>>, f: &Scaffold| {
        texts[f.path.as_path()].as_ref().ok().cloned()
    };
    let writing = !check
        && files
            .iter()
            .any(|f| current(&texts, f).is_some_and(|t| f.state(t.as_deref()) != State::Current));
    let mut drift = 0;
    let mut unreadable = BTreeSet::new();
    let mut drifted: Vec<&Path> = Vec::new();
    for f in files {
        let path = label(f);
        let Some(text) = current(&texts, f) else {
            if unreadable.insert(&f.path)
                && let Err(e) = &texts[f.path.as_path()]
            {
                eprintln!("error    {}: {e}; left as is", f.path.display());
            }
            continue;
        };
        let state = match f.state(text.as_deref()) {
            State::Current if writing && text.as_deref().is_some_and(|t| f.holds_legacy(t)) => {
                State::Differs
            }
            state => state,
        };
        match (state, check) {
            (State::Current, _) => println!("ok       {path}"),
            (state, true) => {
                drift += 1;
                let what = if state == State::Missing {
                    "missing"
                } else {
                    "differs"
                };
                println!("{what:<8} {path}");
                if state == State::Differs && !drifted.contains(&f.path.as_path()) {
                    drifted.push(&f.path);
                }
            }
            (State::Differs, false)
                if f.kind == Kind::Bot && text.as_deref().is_some_and(has_bot_table) =>
            {
                println!("warn     {path} has no `labels = true`, so the bot stays off; set it");
            }
            (State::Differs, false) if f.kind == Kind::Whole && !force => {
                drift += 1;
                println!("differs  {path} (left as is; --force rewrites it)");
            }
            (_, false) => match f.write(root, text.as_deref().unwrap_or_default()) {
                Ok(written) => {
                    println!("wrote    {path}");
                    texts.insert(&f.path, Ok(Some(written)));
                }
                Err(e) => return err(&format!("cannot write {path}: {e}")),
            },
        }
    }
    for path in drifted {
        let scaffolds: Vec<&Scaffold> = files.iter().filter(|f| f.path == path).collect();
        if let Ok(Some(text)) = &texts[path] {
            print!("{}", path_diff(&scaffolds, text));
        }
    }
    if check && drift > 0 {
        println!("init: {drift} file(s) drifted from `cargo soothfast init`; run it to update");
        return 1;
    }
    i32::from(drift > 0 || !unreadable.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::ecosystem::{Ecosystem, parse_projects};
    use crate::init::test_support::{mixed_ws, project, python_ws, scratch, ws};
    use crate::init::workflow::Workspace;
    #[test]
    fn package_ci_proto_and_coverage_tables_are_not_drift() {
        let root = scratch("package-tables");
        let own = "[package.x]\nfeatures = \"all\"\n\n[package.x.gate]\nfeatures = \"bench\"\n\n[docs]\npackage = \"x\"\ncoverage = 80\n\n[ci]\nskip = [\"sdk\"]\n\n[[proto]]\nproto = \"w.proto\"\nmessage = \"W\"\nsource = \"w.rs\"\nstruct = \"W\"\n";
        std::fs::write(root.join("soothfast.toml"), own).unwrap();
        let mut files = scaffold(&ws(&[], Some("x")));
        files.retain(|f| f.path == Path::new("soothfast.toml"));
        assert!(files.len() > 1);
        assert_eq!(report(&root, &files, true, false), 0);
        assert_eq!(report(&root, &files, false, false), 0);
        assert_eq!(
            std::fs::read_to_string(root.join("soothfast.toml")).unwrap(),
            own
        );
    }

    #[test]
    fn labels_scaffold_a_bot_table_once_and_leave_an_existing_one() {
        let root = scratch("bot-table");
        let w = Workspace {
            labels: true,
            ..ws(&[], None)
        };
        let files = scaffold(&w);
        assert_eq!(report(&root, &files, true, false), 1);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert!(text.ends_with("\n\n[bot]\nlabels = true\n"));
        assert_eq!(report(&root, &files, true, false), 0);
        let own = "[bot]\nlabels = true\nmerge_labels = [\"ship\"]\n";
        std::fs::write(root.join("soothfast.toml"), own).unwrap();
        assert_eq!(report(&root, &files, true, false), 0);
        assert_eq!(report(&root, &files, false, false), 0);
        assert_eq!(
            std::fs::read_to_string(root.join("soothfast.toml")).unwrap(),
            own
        );
    }

    #[test]
    fn a_bot_table_without_labels_true_differs_and_is_not_rewritten() {
        let root = scratch("bot-off");
        let own = "[bot]\nmerge_labels = [\"ship\"]\n";
        std::fs::write(root.join("soothfast.toml"), own).unwrap();
        let w = Workspace {
            labels: true,
            ..ws(&[], None)
        };
        let files = scaffold(&w);
        assert_eq!(report(&root, &files, true, false), 1);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert_eq!(text, own);
    }

    #[test]
    fn a_repo_with_a_caller_workflow_still_gets_the_bot_table() {
        let root = scratch("caller-bot");
        let dir = root.join(".github/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ci.yml"),
            "      - uses: Verdenroz/soothfast@abc\n",
        )
        .unwrap();
        let w = Workspace {
            bot: true,
            labels: true,
            ..ws(&[], None)
        };
        let mut files = scaffold(&w);
        files.retain(|f| f.kind == Kind::Bot);
        assert_eq!(files.len(), 1);
        assert_eq!(report(&root, &files, true, false), 1);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert_eq!(text, "[bot]\nlabels = true\n");
        assert!(!root.join(WORKFLOW_PATH).exists());
        assert!(!root.join(".gitignore").exists());
        assert_eq!(report(&root, &files, true, false), 0);
    }

    #[test]
    fn the_docs_packages_are_written_and_a_flagless_check_is_clean() {
        let root = scratch("docs");
        std::fs::write(root.join("soothfast.toml"), "[docs]\npaths = [\"x\"]\n").unwrap();
        let w = Workspace {
            docs_packages: vec!["a".into(), "b".into()],
            ..ws(&[], None)
        };
        let files = scaffold(&w);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert!(text.contains("package = [\"a\",\"b\"]") && text.contains("paths = [\"x\"]"));
        assert_eq!(report(&root, &files, true, false), 0);
    }

    #[test]
    fn a_file_that_is_not_text_is_reported_and_left_alone() {
        let root = scratch("not-utf8");
        std::fs::write(root.join(".gitignore"), [0xff, 0xfe, b'\n']).unwrap();
        let ignore = Scaffold {
            path: ".gitignore".into(),
            kind: Kind::Line,
            content: ".soothfast/".into(),
        };
        assert!(read_text(&root, &ignore.path).is_err());
        assert_eq!(report(&root, &[ignore], true, false), 1);
        let ignore = Scaffold {
            path: ".gitignore".into(),
            kind: Kind::Line,
            content: ".soothfast/".into(),
        };
        assert_eq!(report(&root, &[ignore], false, false), 1);
        assert_eq!(
            std::fs::read(root.join(".gitignore")).unwrap(),
            [0xff, 0xfe, b'\n']
        );
    }

    #[test]
    fn check_reports_drift_and_writes_nothing() {
        let root = scratch("check");
        let files = scaffold(&ws(&["a"], None));
        assert_eq!(report(&root, &files, true, false), 1);
        assert!(!root.join(WORKFLOW_PATH).exists());
        assert_eq!(report(&root, &files, false, false), 0);
        assert_eq!(report(&root, &files, true, false), 0);
    }

    #[test]
    fn the_init_table_makes_a_flagless_check_clean() {
        let root = scratch("table");
        let files = scaffold(&python_ws());
        assert_eq!(report(&root, &files, true, false), 1);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        let entry = parse_projects(&text).unwrap().remove(0);
        let bench = entry.settings.bench(Ecosystem::Python).unwrap();
        assert_eq!(
            (entry.dir.as_str(), bench),
            (".", python_ws().projects.remove(0).bench.unwrap())
        );
        assert_eq!(report(&root, &files, true, false), 0);
    }

    #[test]
    fn a_changed_bench_rewrites_the_table_and_keeps_the_rest() {
        let root = scratch("rewrite");
        std::fs::write(
            root.join("soothfast.toml"),
            "[gate]\nwalltime = \"hard\"\n\n[init]\nfrom = \"pyperf\"\n\n[site]\nname = \"x\"\n",
        )
        .unwrap();
        let files = scaffold(&python_ws());
        assert_eq!(report(&root, &files, true, false), 1);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert!(text.contains("walltime = \"hard\"") && text.contains("name = \"x\""));
        assert_eq!(text.matches("[[project]]").count(), 1);
        assert!(!text.contains("pyperf"));
        assert_eq!(report(&root, &files, true, false), 0);
    }

    #[test]
    fn a_padded_commented_init_header_is_replaced_in_place() {
        let root = scratch("padded");
        std::fs::write(
            root.join("soothfast.toml"),
            "[ init ] # managed\nfrom = \"pyperf\"\n\n[site]\nname = \"x\"\n",
        )
        .unwrap();
        let files = scaffold(&python_ws());
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert_eq!(text.matches("[[project]]").count(), 1);
        assert!(!text.contains("[ init ]") && !text.contains("pyperf"));
        assert!(parse_projects(&text).is_ok());
    }

    #[test]
    fn the_manifest_holds_every_project_and_a_flagless_check_is_clean() {
        let root = scratch("mixed");
        let w = mixed_ws(
            true,
            vec![
                project(Ecosystem::Python, "plugin/x"),
                project(Ecosystem::Gradle, "client"),
            ],
        );
        let files = scaffold(&w);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert_eq!(text.matches("[[project]]").count(), 2);
        assert!(text.contains("walltime = \"advisory\""));
        assert_eq!(report(&root, &files, true, false), 0);
    }

    #[test]
    fn a_legacy_init_table_is_current_but_rewritten_when_init_writes() {
        let root = scratch("legacy");
        let files = scaffold(&python_ws());
        report(&root, &files, false, false);
        let legacy = Settings::default().bench(Ecosystem::Python).unwrap();
        std::fs::write(
            root.join("soothfast.toml"),
            format!(
                "[init]\necosystem = \"python\"\nbench_cmd = \"{}\"\nbench_out = \"{}\"\nfrom = \"{}\"\n",
                legacy.cmd, legacy.out, legacy.from
            ),
        )
        .unwrap();
        assert_eq!(report(&root, &files, true, false), 0);
        std::fs::remove_file(root.join(".gitignore")).unwrap();
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert!(text.contains("[[project]]") && !text.contains("[init]"));
    }

    #[test]
    fn the_runner_and_test_settings_are_saved_and_a_flagless_check_is_clean() {
        let root = scratch("ci");
        std::fs::write(
            root.join("soothfast.toml"),
            "[test]\nignore = [\"docs/**\"]\nci = true\n\n[site]\nname = \"x\"\n",
        )
        .unwrap();
        let w = Workspace {
            ci: Ci {
                labels: Some(vec!["corvid".into()]),
                test: Some(false),
                docs: None,
            },
            ..ws(&["a"], None)
        };
        let files = scaffold(&w);
        assert_eq!(report(&root, &files, true, false), 1);
        assert_eq!(report(&root, &files, false, false), 0);
        let text = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
        assert_eq!(text.matches("[runner]").count(), 1);
        assert_eq!(text.matches("ci = ").count(), 1);
        assert!(text.contains("ignore = [\"docs/**\"]") && text.contains("ci = false"));
        assert!(text.contains("name = \"x\""));
        assert_eq!(report(&root, &files, true, false), 0);
    }

    #[test]
    fn an_edited_workflow_is_kept_unless_forced() {
        let root = scratch("force");
        let files = scaffold(&ws(&[], None));
        report(&root, &files, false, false);
        std::fs::write(root.join(WORKFLOW_PATH), "mine").unwrap();
        assert_eq!(report(&root, &files, false, false), 1);
        assert_eq!(
            std::fs::read_to_string(root.join(WORKFLOW_PATH)).unwrap(),
            "mine"
        );
        assert_eq!(report(&root, &files, false, true), 0);
        assert_ne!(
            std::fs::read_to_string(root.join(WORKFLOW_PATH)).unwrap(),
            "mine"
        );
    }
}
