use std::path::PathBuf;

use super::*;

fn repo(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("soothfast-init-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    invoke::git_in(&root, &["init", "-q"]).unwrap();
    invoke::git_in(&root, &["add", "."]).unwrap();
    root
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| a.to_string()).collect()
}

#[test]
fn init_sets_up_a_repository_with_no_cargo_manifest_and_then_checks_clean() {
    let root = repo(
        "python-only",
        &[(
            "pyproject.toml",
            "[project]\nname = \"app\"\ndependencies = [\"pytest-benchmark\"]\n",
        )],
    );
    assert_eq!(run_at(&root, &args(&["--no-bot"])), 0);
    for file in [WORKFLOW_PATH, "soothfast.toml", ".gitignore"] {
        assert!(root.join(file).is_file(), "{file} not written");
    }
    let toml = std::fs::read_to_string(root.join("soothfast.toml")).unwrap();
    assert!(toml.contains("[[project]]") && toml.contains("ecosystem = \"python\""));
    assert_eq!(run_at(&root, &args(&["--no-bot", "--check"])), 0);
}
