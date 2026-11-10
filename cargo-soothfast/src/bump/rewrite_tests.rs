use std::cell::RefCell;
use std::path::Path;

use super::tests::{NEW, OLD, check, fixture, read, sha};
use super::*;

fn bumped(root: &Path, resolved: Option<String>) -> Result<Vec<String>, String> {
    let resolve = move |_: &str, _: &str| resolved.clone();
    let update = |_: &Path, _: &[(String, String)], _: &str| Ok(());
    bump(
        root,
        NEW,
        &Tools {
            resolve: &resolve,
            update_lock: &update,
        },
    )
}

#[test]
fn a_manifest_is_rewritten_and_the_lock_follows_for_just_those_packages() {
    let lock = format!(
        "[[package]]\nname = \"soothfast\"\nversion = \"{OLD}\"\nsource = \"registry+x\"\n\n\
         [[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+x\"\n"
    );
    let root = fixture(
        "rewrite-cargo",
        &[
            (
                "Cargo.toml",
                &format!(
                    "[dependencies]\nsoothfast = {{ version = \"~{OLD}\", features = [\"runner\"] }}\n"
                ),
            ),
            ("Cargo.lock", &lock),
        ],
    );
    let moved = RefCell::new(Vec::new());
    let update = |root: &Path, packages: &[(String, String)], target: &str| {
        moved.borrow_mut().extend_from_slice(packages);
        let lock = read(root, "Cargo.lock").replace(OLD, target);
        std::fs::write(root.join("Cargo.lock"), lock).map_err(|e| e.to_string())
    };
    let resolve = |_: &str, _: &str| None;
    let tools = Tools {
        resolve: &resolve,
        update_lock: &update,
    };
    assert_eq!(
        bump(&root, NEW, &tools).unwrap(),
        ["Cargo.toml", "Cargo.lock"]
    );
    assert_eq!(
        read(&root, "Cargo.toml"),
        "[dependencies]\nsoothfast = { version = \"~0.5.7\", features = [\"runner\"] }\n"
    );
    assert_eq!(
        *moved.borrow(),
        [("soothfast".to_string(), OLD.to_string())]
    );
    assert_eq!(check(&root), (Vec::new(), false));
}

#[test]
fn workflow_refs_move_to_the_resolved_commit_with_their_comments() {
    let workflow = format!(
        "jobs:\n  a:\n    uses: acme/soothfast/.github/workflows/run.yml@{} # v{OLD}\n    with:\n      \
         soothfast_ref: \"{}\"\n      bot: true\n  b:\n    steps:\n      \
         - uses: acme/soothfast@{}\n      - uses: acme/soothfast@v{OLD}\n      \
         - run: cargo install cargo-soothfast --version {OLD} --locked\n",
        sha('a'),
        sha('a'),
        sha('c'),
    );
    let root = fixture(
        "rewrite-workflow",
        &[(".github/workflows/ci.yml", &workflow)],
    );
    let changed = bumped(&root, Some(sha('f'))).unwrap();
    assert_eq!(changed, [".github/workflows/ci.yml"]);
    assert_eq!(
        read(&root, ".github/workflows/ci.yml"),
        format!(
            "jobs:\n  a:\n    uses: acme/soothfast/.github/workflows/run.yml@{f} # v{NEW}\n    with:\n      \
             soothfast_ref: \"{f}\"\n      bot: true\n  b:\n    steps:\n      \
             - uses: acme/soothfast@{f} # v{NEW}\n      - uses: acme/soothfast@v{NEW}\n      \
             - run: cargo install cargo-soothfast --version {NEW} --locked\n",
            f = sha('f')
        )
    );
    assert_eq!(check(&root), (Vec::new(), false));
}

#[test]
fn a_file_with_nothing_to_change_is_left_alone_and_resolves_nothing() {
    let text = format!(
        "jobs:\n  a:\n    uses: acme/soothfast@{} # v{NEW}\n    with:\n      soothfast_ref: \"{}\"\n",
        sha('f'),
        sha('f')
    );
    let root = fixture("rewrite-clean", &[(".github/workflows/ci.yml", &text)]);
    assert_eq!(bumped(&root, None).unwrap(), Vec::<String>::new());
    assert_eq!(read(&root, ".github/workflows/ci.yml"), text);
}

#[test]
fn an_unresolvable_tag_fails_before_anything_is_written() {
    let manifest = "[dependencies]\nsoothfast = \"0.4\"\n".to_string();
    let workflow = format!(
        "jobs:\n  a:\n    uses: acme/soothfast@{} # v{OLD}\n",
        sha('a')
    );
    let root = fixture(
        "rewrite-unresolved",
        &[
            ("Cargo.toml", &manifest),
            (".github/workflows/ci.yml", &workflow),
        ],
    );
    let err = bumped(&root, None).unwrap_err();
    assert_eq!(err, format!("could not resolve v{NEW} in acme/soothfast"));
    assert_eq!(read(&root, "Cargo.toml"), manifest);
    assert_eq!(read(&root, ".github/workflows/ci.yml"), workflow);
}

#[test]
fn a_prerelease_target_round_trips_through_bump_and_check() {
    let workflow = format!(
        "jobs:\n  a:\n    uses: acme/soothfast@{} # v{OLD}\n",
        sha('a')
    );
    let root = fixture("rewrite-pre", &[(".github/workflows/ci.yml", &workflow)]);
    let resolve = |_: &str, tag: &str| (tag == "v0.6.0-rc.1").then(|| sha('e'));
    let update = |_: &Path, _: &[(String, String)], _: &str| Ok(());
    let tools = Tools {
        resolve: &resolve,
        update_lock: &update,
    };
    bump(&root, "0.6.0-rc.1", &tools).unwrap();
    let scanned = scan_repo(&root, "0.6.0-rc.1").unwrap();
    assert_eq!(findings(&scanned, "0.6.0-rc.1"), (Vec::new(), false));
}
