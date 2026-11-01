use super::*;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn splits_unit_names() {
    assert_eq!(
        split_unit("serde_json-0123456789abcdef"),
        Some(("serde_json", "0123456789abcdef"))
    );
    assert_eq!(
        split_unit("aws-lc-sys-0123456789abcdef"),
        Some(("aws-lc-sys", "0123456789abcdef"))
    );
    assert_eq!(split_unit("build-script-build"), None);
    assert_eq!(split_unit("nohash"), None);
}

fn profile_dir(root: &Path, units: &[(&str, &[&str])]) {
    for (unit, files) in units {
        fs::create_dir_all(root.join(".fingerprint").join(unit)).unwrap();
        fs::write(root.join(".fingerprint").join(unit).join("lib"), unit).unwrap();
        for file in *files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, unit).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn a_unit_that_cannot_be_copied_is_skipped_without_its_fingerprint() {
    let root = std::env::temp_dir().join(format!("soothfast-seed-skip-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (head, base) = (root.join("head"), root.join("base"));
    profile_dir(
        &head,
        &[
            (
                "good-0123456789abcdef",
                &["deps/libgood-0123456789abcdef.rlib"],
            ),
            (
                "bad-fedcba9876543210",
                &["build/bad-fedcba9876543210/out/a"],
            ),
        ],
    );
    std::os::unix::fs::symlink("a", head.join("build/bad-fedcba9876543210/out/l")).unwrap();

    let seeded = seed(&head, &base, &BTreeSet::new()).unwrap();
    assert_eq!(seeded.units, 1);
    assert_eq!(seeded.skipped.len(), 1, "{:?}", seeded.skipped);
    assert!(base.join(".fingerprint/good-0123456789abcdef").exists());
    assert!(!base.join(".fingerprint/bad-fedcba9876543210").exists());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_per_package_layout_seeds_dependencies_but_not_members() {
    let root = std::env::temp_dir().join(format!("soothfast-seed-nest-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (head, base) = (root.join("head"), root.join("base"));
    for unit in ["serde/0123456789abcdef", "app/fedcba9876543210"] {
        for sub in ["out/x", "fingerprint/lib"] {
            let path = head.join("build").join(unit).join(sub);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, unit).unwrap();
        }
    }
    let members = BTreeSet::from(["app".to_string()]);
    let seeded = seed(&head, &base, &members).unwrap();
    assert_eq!((seeded.units, seeded.skipped.len()), (1, 0), "{seeded:?}");
    assert!(
        base.join("build/serde/0123456789abcdef/fingerprint/lib")
            .exists()
    );
    assert!(base.join("build/serde/0123456789abcdef/out/x").exists());
    assert!(!base.join("build/app").exists());
    assert_eq!(seed(&head, &base, &members).unwrap().units, 0);
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn an_unreadable_package_directory_is_skipped_not_fatal() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("soothfast-seed-deny-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (head, base) = (root.join("head"), root.join("base"));
    for unit in ["locked/0123456789abcdef", "open/fedcba9876543210"] {
        let path = head.join("build").join(unit).join("out/x");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, unit).unwrap();
    }
    let locked = head.join("build/locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

    let seeded = seed(&head, &base, &BTreeSet::new()).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let readable = fs::read_dir(&locked).is_ok();
    assert_eq!(seeded.units, 1);
    assert_eq!(seeded.unreadable.len(), 1, "{seeded:?}");
    assert!(seeded.skipped.is_empty(), "{seeded:?}");
    assert!(readable);
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn read_only_files_seed_and_reseed() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("soothfast-seed-ro-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (head, base) = (root.join("head"), root.join("base"));
    let file = "build/ro-0123456789abcdef/out/gen.rs";
    profile_dir(&head, &[("ro-0123456789abcdef", &[file])]);
    fs::set_permissions(head.join(file), fs::Permissions::from_mode(0o444)).unwrap();

    assert_eq!(seed(&head, &base, &BTreeSet::new()).unwrap().units, 1);
    fs::remove_dir_all(base.join(".fingerprint")).unwrap();
    let again = seed(&head, &base, &BTreeSet::new()).unwrap();
    assert_eq!((again.units, again.skipped.len()), (1, 0), "{again:?}");
    let _ = fs::remove_dir_all(&root);
}

/// The version this workspace's `Cargo.lock` holds for `name`, which an
/// `--offline` build can resolve because the workspace build fetched it.
fn locked(name: &str) -> String {
    let lock = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../Cargo.lock")).unwrap();
    let entry = format!("name = \"{name}\"\nversion = \"");
    let start = lock.find(&entry).unwrap() + entry.len();
    lock[start..].split('"').next().unwrap().to_string()
}

fn write_workspace(root: &Path, deps: &str, body: &str) {
    let files = [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"m\"]\nresolver = \"2\"\n".to_string(),
        ),
        (
            "m/Cargo.toml",
            format!(
                "[package]\nname = \"m\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}"
            ),
        ),
        ("m/src/lib.rs", body.to_string()),
    ];
    for (path, text) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

/// `cargo build --release` in `root`; each unit's (name, fresh).
fn build(root: &Path, target: &Path) -> Vec<(String, bool)> {
    let out = Command::new("cargo")
        .args(["build", "--release", "--offline", "--message-format=json"])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", target)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    crate::invoke::cargo_messages(&out.stdout)
        .filter(|m| m["reason"] == "compiler-artifact")
        .map(|m| {
            let name = m["target"]["name"].as_str().unwrap_or_default().to_string();
            (name, m["fresh"].as_bool().unwrap_or(false))
        })
        .collect()
}

fn tree(dir: &Path, out: &mut BTreeMap<PathBuf, (u64, std::time::SystemTime)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let meta = fs::symlink_metadata(entry.path()).unwrap();
        if meta.is_dir() {
            tree(&entry.path(), out);
        } else {
            let bytes = fs::read(entry.path()).unwrap_or_default();
            let hash = soothfast_registry::fnv1a(&bytes);
            out.insert(entry.path(), (hash ^ meta.len(), meta.modified().unwrap()));
        }
    }
}

/// The rustc invocation `cargo build -v` printed for crate `name`.
fn rustc_args(verbose: &str, name: &str) -> Vec<String> {
    let line = verbose
        .lines()
        .find(|l| l.contains(&format!("--crate-name {name} ")))
        .unwrap();
    let cmd = &line[line.find('`').unwrap() + 1..line.rfind('`').unwrap()];
    let (mut args, mut word, mut quoted) = (Vec::new(), String::new(), false);
    for c in cmd.chars() {
        match c {
            '\'' => quoted = !quoted,
            ' ' if !quoted => args.push(std::mem::take(&mut word)),
            c => word.push(c),
        }
    }
    args.push(word);
    args.retain(|a| !a.is_empty());
    args
}

/// Rebuild `name`'s unit in place under another `-C metadata`: the same
/// unit and fingerprint, a different crate hash, as two trees can build. The
/// `verbose` build wrote to `built`; the rebuild writes to `deps`.
fn rebuild_under_other_metadata(
    verbose: &str,
    name: &str,
    built: &Path,
    deps: &Path,
    keep_mtimes: bool,
) {
    let (from, to) = (built.to_string_lossy(), deps.to_string_lossy());
    let args: Vec<String> = rustc_args(verbose, name)
        .into_iter()
        .map(|a| match a.strip_prefix("metadata=") {
            Some(_) => "metadata=00000000000000ff".to_string(),
            None => a.replace(from.as_ref(), to.as_ref()),
        })
        .collect();
    let outputs: Vec<(PathBuf, std::time::SystemTime)> = fs::read_dir(deps)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("lib{name}-"))
        })
        .map(|e| (e.path(), e.metadata().unwrap().modified().unwrap()))
        .collect();
    let out = Command::new(&args[0]).args(&args[1..]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for (path, mtime) in outputs.into_iter().filter(|_| keep_mtimes) {
        fs::File::open(path).unwrap().set_modified(mtime).unwrap();
    }
}

/// `cargo build --release -v` in `root`; what it printed to stderr.
fn build_verbose(root: &Path, target: &Path) -> String {
    let out = Command::new("cargo")
        .args(["build", "--release", "--offline", "-v"])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", target)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_unit_built_on_a_dependency_the_base_built_itself_is_not_seeded() {
    let root = std::env::temp_dir().join(format!("soothfast-seed-mixed-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (head, base) = (root.join("head"), root.join("base"));
    let (head_t, base_t) = (root.join("head-target"), root.join("base-target"));
    let pin = |name: &str| format!("{name} = \"={}\"\n", locked(name));
    let uses = "pub fn f() -> bool { aho_corasick::AhoCorasick::new([\"a\"]).is_ok() }\n";
    let both = format!("{}{}", pin("memchr"), pin("aho-corasick"));
    write_workspace(&head, &both, uses);
    write_workspace(&base, &pin("memchr"), "pub fn f() -> u8 { 2 }\n");

    let verbose = build_verbose(&base, &base_t);
    let deps = base_t.join("release/deps");
    rebuild_under_other_metadata(&verbose, "memchr", &deps, &deps, true);
    write_workspace(&base, &both, uses);
    build(&head, &head_t);

    let members = BTreeSet::from(["m".to_string()]);
    let seeded = seed(&head_t.join("release"), &base_t.join("release"), &members).unwrap();
    let units = build(&base, &base_t);
    let fresh = |name: &str| units.iter().find(|(n, _)| n == name).map(|(_, f)| *f);
    assert_eq!(fresh("aho_corasick"), Some(false), "{units:?}");
    assert!(
        seeded
            .skipped
            .iter()
            .any(|s| s.starts_with("aho-corasick-")),
        "{seeded:?}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn base_build_reuses_seeded_dependencies_and_leaves_head_alone() {
    let root = std::env::temp_dir().join(format!("soothfast-seed-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (head, base) = (root.join("head"), root.join("base"));
    let (head_t, base_t) = (root.join("head-target"), root.join("base-target"));
    let pin = |name: &str| format!("{name} = \"={}\"\n", locked(name));
    let shared = format!("{}{}", pin("itoa"), pin("libc"));
    write_workspace(&head, &shared, "pub fn f() -> u8 { 1 }\n");
    write_workspace(
        &base,
        &format!("{shared}{}", pin("memchr")),
        "pub fn f() -> u8 { 2 }\n",
    );

    build(&head, &head_t);
    let mut before = BTreeMap::new();
    tree(&head_t, &mut before);

    let members = BTreeSet::from(["m".to_string()]);
    let seeded = seed(&head_t.join("release"), &base_t.join("release"), &members).unwrap();
    assert!(seeded.units >= 2, "{seeded:?}");
    let units = build(&base, &base_t);
    let fresh = |name: &str| units.iter().find(|(n, _)| n == name).map(|(_, f)| *f);
    assert_eq!(fresh("itoa"), Some(true), "{units:?}");
    assert_eq!(fresh("libc"), Some(true), "{units:?}");
    assert_eq!(fresh("memchr"), Some(false), "only base depends on it");
    assert_eq!(fresh("m"), Some(false), "a member is never seeded");

    let mut after = BTreeMap::new();
    tree(&head_t, &mut after);
    assert_eq!(before, after, "the base build changed HEAD's target dir");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_unit_an_earlier_seed_copied_does_not_block_its_dependents() {
    let root = std::env::temp_dir().join(format!("soothfast-seed-again-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (head, base) = (root.join("head"), root.join("base"));
    let (head_t, base_t) = (root.join("head-target"), root.join("base-target"));
    let pin = |name: &str| format!("{name} = \"={}\"\n", locked(name));
    write_workspace(&head, &pin("memchr"), "pub fn f() -> u8 { 1 }\n");
    write_workspace(&base, &pin("memchr"), "pub fn f() -> u8 { 2 }\n");
    build(&head, &head_t);
    let members = BTreeSet::from(["m".to_string()]);
    let (head_p, base_p) = (head_t.join("release"), base_t.join("release"));
    let first = seed(&head_p, &base_p, &members).unwrap();
    assert!(
        first.names.iter().any(|n| n.starts_with("memchr-")),
        "{first:?}"
    );
    build(&base, &base_t);

    let both = format!("{}{}", pin("memchr"), pin("aho-corasick"));
    let uses = "pub fn f() -> bool { aho_corasick::AhoCorasick::new([\"a\"]).is_ok() }\n";
    write_workspace(&head, &both, uses);
    write_workspace(&base, &both, uses);
    build(&head, &head_t);
    let second = seed(&head_p, &base_p, &members).unwrap();
    assert!(
        second.names.iter().any(|n| n.starts_with("aho-corasick-")),
        "{second:?}"
    );
    let units = build(&base, &base_t);
    let fresh = |name: &str| units.iter().find(|(n, _)| n == name).map(|(_, f)| *f);
    assert_eq!(fresh("aho_corasick"), Some(true), "{units:?}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_seeded_unit_rebuilt_in_place_leaves_its_dependents_to_the_base() {
    let root = std::env::temp_dir().join(format!("soothfast-seed-rebuilt-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (head, base) = (root.join("head"), root.join("base"));
    let (head_t, base_t) = (root.join("head-target"), root.join("base-target"));
    let pin = |name: &str| format!("{name} = \"={}\"\n", locked(name));
    write_workspace(&head, &pin("memchr"), "pub fn f() -> u8 { 1 }\n");
    write_workspace(&base, &pin("memchr"), "pub fn f() -> u8 { 2 }\n");
    let verbose = build_verbose(&head, &head_t);
    let members = BTreeSet::from(["m".to_string()]);
    let (head_p, base_p) = (head_t.join("release"), base_t.join("release"));
    let first = seed(&head_p, &base_p, &members).unwrap();
    assert!(
        first.names.iter().any(|n| n.starts_with("memchr-")),
        "{first:?}"
    );
    let (built, deps) = (head_p.join("deps"), base_p.join("deps"));
    rebuild_under_other_metadata(&verbose, "memchr", &built, &deps, false);

    let both = format!("{}{}", pin("memchr"), pin("aho-corasick"));
    let uses = "pub fn f() -> bool { aho_corasick::AhoCorasick::new([\"a\"]).is_ok() }\n";
    write_workspace(&head, &both, uses);
    write_workspace(&base, &both, uses);
    build(&head, &head_t);
    let second = seed(&head_p, &base_p, &members).unwrap();
    let units = build(&base, &base_t);
    let fresh = |name: &str| units.iter().find(|(n, _)| n == name).map(|(_, f)| *f);
    assert_eq!(fresh("aho_corasick"), Some(false), "{units:?}");
    assert!(
        second
            .skipped
            .iter()
            .any(|s| s.starts_with("aho-corasick-")),
        "{second:?}"
    );
    let _ = fs::remove_dir_all(&root);
}
