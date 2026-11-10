use std::path::PathBuf;

use super::*;

const OLD: &str = "0.5.5";
const NEW: &str = "0.5.7";

fn fixture(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("soothfast-bump-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    root
}

fn sha(c: char) -> String {
    c.to_string().repeat(40)
}

fn check(root: &Path) -> (Vec<String>, bool) {
    findings(&scan_repo(root, NEW).unwrap(), NEW)
}

#[test]
fn a_stale_cargo_requirement_is_reported_in_each_dependency_form() {
    let root = fixture(
        "cargo",
        &[(
            "crates/a/Cargo.toml",
            &format!(
                "[package]\nname = \"a\"\nrust-version = \"1.88\"\n\n\
                 [dependencies]\nsoothfast = \"{OLD}\"\n\
                 soothfast-registry = {{ path = \"../r\", version = \"^{OLD}\" }}\n\
                 soothfast-docs = {{ path = \"../d\" }}\n\
                 serde = \"1.0.1\"\n\n\
                 [dev-dependencies.soothfast-measure]\nversion = \"={OLD}\"\n"
            ),
        )],
    );
    let (lines, stale) = check(&root);
    assert!(stale);
    assert_eq!(
        lines,
        [
            format!("crates/a/Cargo.toml:6  found {OLD}, want {NEW}"),
            format!("crates/a/Cargo.toml:7  found {OLD}, want {NEW}"),
            format!("crates/a/Cargo.toml:12  found {OLD}, want {NEW}"),
        ]
    );
}

#[test]
fn a_requirement_on_the_target_is_not_stale() {
    let root = fixture(
        "cargo-current",
        &[(
            "Cargo.toml",
            &format!("[workspace.dependencies]\nsoothfast = \"{NEW}\"\n"),
        )],
    );
    assert_eq!(check(&root), (Vec::new(), false));
}

#[test]
fn a_lock_entry_behind_the_target_is_reported() {
    let lock = format!(
        "[[package]]\nname = \"soothfast\"\nversion = \"{OLD}\"\nsource = \"registry+x\"\n\n\
         [[package]]\nname = \"soothfast-docs\"\nversion = \"{OLD}\"\nsource = \"registry+x\"\n"
    );
    let root = fixture(
        "lock",
        &[
            (
                "Cargo.toml",
                &format!("[dependencies]\nsoothfast = \"{NEW}\"\n"),
            ),
            ("Cargo.lock", &lock),
        ],
    );
    assert_eq!(
        check(&root),
        (vec![format!("Cargo.lock:3  found {OLD}, want {NEW}")], true)
    );
}

#[test]
fn a_uses_ref_is_stale_by_its_version_comment_whatever_the_owner() {
    let workflow = format!(
        "jobs:\n  a:\n    uses: acme/soothfast/.github/workflows/run.yml@{} # v{OLD}\n\
         \x20 b:\n    steps:\n      - uses: other/soothfast@{} # v{NEW}\n\
         \x20     - uses: actions/checkout@{} # v4.2.2\n\
         \x20     - uses: ./.github/workflows/soothfast.yml\n\
         \x20     - uses: acme/soothfast@v{OLD}\n\
         \x20     - uses: acme/soothfast@{}\n",
        sha('a'),
        sha('b'),
        sha('c'),
        sha('d'),
    );
    let root = fixture("uses", &[(".github/workflows/ci.yml", &workflow)]);
    assert_eq!(
        check(&root).0,
        [
            format!(".github/workflows/ci.yml:3  found {OLD}, want {NEW}"),
            format!(".github/workflows/ci.yml:9  found {OLD}, want {NEW}"),
            format!(".github/workflows/ci.yml:10  found ddddddd, want {NEW}"),
        ]
    );
}

#[test]
fn a_soothfast_ref_is_stale_unless_it_is_the_commit_of_a_current_uses() {
    let current = format!(
        "uses: a/soothfast/.github/workflows/run.yml@{} # v{NEW}\n    with:\n      soothfast_ref: \"{}\"\n",
        sha('b'),
        sha('b'),
    );
    let stale = format!(
        "uses: a/soothfast/.github/workflows/run.yml@{} # v{NEW}\n    with:\n      soothfast_ref: \"{}\"\n",
        sha('b'),
        sha('a'),
    );
    let root = fixture(
        "ref",
        &[
            (".github/workflows/one.yml", &current),
            (".github/workflows/two.yml", &stale),
            (
                ".github/workflows/three.yaml",
                "jobs:\n  g:\n    with:\n      soothfast_ref: ${{ github.sha }}\n",
            ),
        ],
    );
    assert_eq!(
        check(&root).0,
        [".github/workflows/two.yml:3  found aaaaaaa, want 0.5.7"]
    );
}

#[test]
fn literal_install_versions_are_pins_and_other_forms_are_unrecognised() {
    let workflow = format!(
        "    steps:\n      - run: cargo install cargo-soothfast --version {OLD} --locked\n\
         \x20     - run: cargo binstall cargo-soothfast@{OLD}\n\
         \x20     - run: cargo install cargo-soothfast --version \"{NEW}\"\n\
         \x20     - run: cargo install cargo-soothfast --version \"$V\"\n\
         \x20     - run: cargo install cargo-soothfast --locked\n\
         \x20     - run: cargo install ripgrep --version 14.1.0\n"
    );
    let root = fixture("install", &[(".github/workflows/i.yml", &workflow)]);
    let (lines, stale) = check(&root);
    assert!(stale);
    assert_eq!(
        lines,
        [
            format!(".github/workflows/i.yml:2  found {OLD}, want {NEW}"),
            format!(".github/workflows/i.yml:3  found {OLD}, want {NEW}"),
            ".github/workflows/i.yml:5  unrecognised cargo-soothfast pin: \
             - run: cargo install cargo-soothfast --version \"$V\""
                .to_string(),
        ]
    );
}

#[test]
fn a_repository_with_nothing_to_change_is_clean() {
    let root = fixture(
        "clean",
        &[
            ("Cargo.toml", "[dependencies]\nserde = \"1\"\n"),
            (
                ".github/workflows/ci.yml",
                "jobs:\n  t:\n    steps:\n      - uses: actions/checkout@v4\n",
            ),
        ],
    );
    assert_eq!(check(&root), (Vec::new(), false));
}
