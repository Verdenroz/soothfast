use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::ci::checks::Check;
use crate::init::ecosystem::Ecosystem;
use crate::lanes::store::Pass;
use crate::plan::tests::{plan, project};
use crate::test::config::TestConfig;
use crate::test::fixture::{TempDir, TempRepo};
use crate::test::project::{Pins, TEST_ALL_LABEL};

const PASSED: &str = "0123456789abcdef0123456789abcdef01234567";

fn repo(tag: &str) -> TempRepo {
    let repo = TempRepo::new(tag);
    for f in [
        "core/src/lib.rs",
        "cli/src/lib.rs",
        "web/a.js",
        "docs/page.md",
        "docs/data.txt",
        "Cargo.lock",
    ] {
        repo.write(f, "1");
    }
    repo.git(&["add", "."]);
    repo
}

fn graph(root: &Path) -> Graph {
    let pkg = |name: &str, deps: &[&str]| {
        json!({
            "name": name,
            "manifest_path": format!("{}/{name}/Cargo.toml", root.display()),
            "dependencies": deps.iter().map(|d| json!({ "name": d })).collect::<Vec<_>>(),
        })
    };
    let meta = json!({ "packages": [pkg("core", &[]), pkg("cli", &["core"])] });
    Graph::from_metadata(&meta, root).unwrap()
}

fn lanes() -> Plan {
    plan(
        Some(&["core", "cli"]),
        vec![project(Ecosystem::Node, "web")],
    )
}

fn site() -> Site {
    Site {
        reads: vec![Reads::Under("book".into())],
        hook: false,
    }
}

fn units(p: &Plan, repo: &TempRepo, site: Option<&Site>) -> Units {
    let files = Files::read(&repo.root).unwrap();
    p.units(&inputs(&files, repo), site, &|_| Some(String::new()))
}

/// A stand-in for the base side of the changed files a lane reads: empty when
/// it reads none, else named by `base` and the files.
fn delta<'a>(
    inputs: &'a Inputs,
    changed: &'a [&'a str],
    base: &'a str,
) -> impl Fn(&Lane) -> Option<String> + 'a {
    move |lane| {
        let read: Vec<&str> = changed
            .iter()
            .copied()
            .filter(|f| inputs.reads(lane, f))
            .collect();
        Some(if read.is_empty() {
            String::new()
        } else {
            format!("{base}:{}", read.join(","))
        })
    }
}

fn inputs<'a>(files: &'a Files, repo: &TempRepo) -> Inputs<'a> {
    let test = TestConfig {
        ignore: vec!["docs/data.txt".into()],
        ..TestConfig::default()
    };
    let setup = Setup {
        tool: "t1".into(),
        rust_stable: "1.90".into(),
        runner: "\"ubuntu-latest\"".into(),
        shards: 1,
        pins: Pins::default(),
    };
    Inputs::new(files.ids(), Some(graph(&repo.root)), test, vec![], setup).unwrap()
}

fn pass_all(p: &Plan, repo: &TempRepo, site: Option<&Site>) -> (TempDir, Store) {
    let tag = repo.root.file_name().unwrap().to_string_lossy().to_string();
    let dir = TempDir::new(&format!("{tag}-store"));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::at(dir.to_path_buf());
    for u in units(p, repo, site).all {
        let pass = Pass {
            commit: PASSED.into(),
            run: "1".into(),
        };
        store.record(&u.lane.id, &u.key, &pass);
    }
    (dir, store)
}

fn pull(p: &mut Plan, repo: &TempRepo, store: &Store, site: Option<&Site>, changed: &[&str]) {
    pull_on(p, repo, store, site, changed, "b1");
}

fn pull_on(
    p: &mut Plan,
    repo: &TempRepo,
    store: &Store,
    site: Option<&Site>,
    changed: &[&str],
    base: &str,
) {
    p.changed = Some(changed.iter().map(|c| c.to_string()).collect());
    let files = Files::read(&repo.root).unwrap();
    let inputs = inputs(&files, repo);
    p.decide(&inputs, site, Some(store), &delta(&inputs, changed, base));
}

/// The pass a pull request head stores for its lanes under its own deltas.
fn pass_head(repo: &TempRepo, store: &Store, changed: &[&str], base: &str) {
    let files = Files::read(&repo.root).unwrap();
    let inputs = inputs(&files, repo);
    let units = lanes().units(&inputs, None, &delta(&inputs, changed, base));
    for u in units.all {
        let pass = Pass {
            commit: "feedface".into(),
            run: "2".into(),
        };
        store.record(&u.lane.id, &u.key, &pass);
    }
}

fn carried(p: &Plan) -> Vec<&str> {
    p.carried.iter().map(|c| c.lane.as_str()).collect()
}

#[test]
fn an_untouched_lane_with_a_stored_pass_leaves_the_outputs() {
    let repo = repo("carry-packages");
    let mut p = lanes();
    let (_dir, store) = pass_all(&p, &repo, None);
    repo.write("cli/src/lib.rs", "2");
    pull(&mut p, &repo, &store, None, &["cli/src/lib.rs"]);
    assert_eq!(carried(&p), ["rust:core", "import:node"]);
    let out = p.outputs();
    assert!(
        out.contains("projects=false\nrust=true\ntest=true\npackages=cli\n"),
        "{out}"
    );
    assert_eq!(p.matrix(false), json!({ "include": [] }));
}

#[test]
fn a_lane_that_read_a_changed_file_runs_even_with_a_matching_pass() {
    let repo = repo("carry-reverted");
    let mut p = lanes();
    let (_dir, store) = pass_all(&p, &repo, None);
    pull(&mut p, &repo, &store, None, &["core/src/lib.rs"]);
    assert_eq!(carried(&p), ["import:node"]);
    assert!(p.outputs().contains("packages=core cli\n"));
}

#[test]
fn a_lane_with_no_stored_pass_runs() {
    let repo = repo("carry-no-pass");
    let mut p = lanes();
    let dir = TempDir::new("plan-carry-empty");
    std::fs::create_dir_all(&dir).unwrap();
    pull(&mut p, &repo, &Store::at(dir.to_path_buf()), None, &[]);
    assert!(p.carried.is_empty());
    assert!(
        p.outputs()
            .contains("projects=true\nrust=true\ntest=true\npackages=core cli\n")
    );
}

#[test]
fn a_changed_project_file_keeps_the_matrix() {
    let repo = repo("carry-matrix");
    let mut p = lanes();
    let (_dir, store) = pass_all(&p, &repo, None);
    repo.write("web/a.js", "2");
    pull(&mut p, &repo, &store, None, &["web/a.js"]);
    assert!(carried(&p).is_empty());
    assert!(p.outputs().contains("projects=true\n"));
}

fn checked(kinds: &[(&'static str, Option<&str>)]) -> Plan {
    let mut p = lanes();
    p.checks = kinds
        .iter()
        .map(|(kind, package)| Check {
            kind,
            package: package.map(String::from),
            rustdoc: true,
        })
        .collect();
    p
}

#[test]
fn a_carried_check_leaves_the_matrix_but_is_still_expected() {
    let repo = repo("carry-checks");
    let mut p = checked(&[("spec", Some("core")), ("spec", Some("cli"))]);
    let (_dir, store) = pass_all(&p, &repo, None);
    repo.write("cli/src/lib.rs", "2");
    pull(&mut p, &repo, &store, None, &["cli/src/lib.rs"]);
    assert!(p.carries("check:spec-core") && !p.carries("check:spec-cli"));
    let out = p.outputs();
    assert!(
        out.contains("checks_any=true\nchecks_expect=spec-core spec-cli\n"),
        "{out}"
    );
    assert_eq!(
        p.matrix_checks().map(Check::name).collect::<Vec<_>>(),
        ["spec-cli"]
    );
    repo.write("cli/src/lib.rs", "1");
    pull(&mut p, &repo, &store, None, &["docs/data.txt"]);
    let out = p.outputs();
    assert!(
        out.contains(
            "checks={\"include\":[]}\nchecks_any=false\nchecks_expect=spec-core spec-cli\n"
        ),
        "{out}"
    );
}

#[test]
fn a_carried_lane_leaves_a_settled_pass_naming_its_commit() {
    let repo = repo("carry-settled");
    let mut p = checked(&[("spec", Some("core"))]);
    let (_dir, store) = pass_all(&p, &repo, None);
    pull(&mut p, &repo, &store, None, &["docs/data.txt"]);
    let out = TempDir::new("plan-carry-settled");
    p.write_settled(&out).unwrap();
    let read = |name: &str| -> Value {
        serde_json::from_str(&std::fs::read_to_string(out.join(name)).unwrap()).unwrap()
    };
    let spec = read("carried-spec-core.json");
    assert_eq!(spec["verdict"], "pass");
    assert_eq!(spec["kind"], "check");
    assert_eq!(spec["carried"], PASSED);
    assert_eq!(read("carried-core.json")["kind"], "rust");
    assert_eq!(read("carried-node.json")["kind"], "import");
}

fn documented(checks: &[(&'static str, Option<&str>)]) -> Plan {
    let mut p = checked(checks);
    p.docs = true;
    p.docs_binds_rust = true;
    p
}

#[test]
fn the_gates_and_the_docs_carry_together_or_not_at_all() {
    let repo = repo("carry-coupled");
    let mut p = documented(&[]);
    let (_dir, store) = pass_all(&p, &repo, None);
    pull(&mut p, &repo, &store, None, &["docs/data.txt"]);
    assert_eq!(
        carried(&p),
        ["rust:core", "rust:cli", "import:node", "docs"]
    );
    let out = p.outputs();
    assert!(out.contains("packages=\ndocs=false\n"), "{out}");
    assert!(out.contains("docs_binds_rust=false\n"), "{out}");

    let mut p = documented(&[]);
    repo.write("docs/page.md", "2");
    pull(&mut p, &repo, &store, None, &["docs/page.md"]);
    assert!(p.carried.is_empty());
    let out = p.outputs();
    assert!(out.contains("packages=core cli\ndocs=true\n"), "{out}");
}

#[test]
fn a_gate_that_cannot_carry_keeps_the_docs_and_every_other_gate() {
    let repo = repo("carry-coupled-gate");
    let mut p = documented(&[]);
    let (_dir, store) = pass_all(&p, &repo, None);
    repo.write("cli/src/lib.rs", "2");
    pull(&mut p, &repo, &store, None, &["cli/src/lib.rs"]);
    assert!(p.carried.is_empty());
    assert!(p.outputs().contains("packages=core cli\ndocs=true\n"));
}

#[test]
fn the_site_in_the_docs_job_is_carried_with_it() {
    let repo = repo("carry-site");
    let mut p = documented(&[("site", None)]);
    let (_dir, store) = pass_all(&p, &repo, Some(&site()));
    pull(&mut p, &repo, &store, Some(&site()), &["docs/data.txt"]);
    assert!(p.carries("docs"));
    let out = p.outputs();
    assert!(out.ends_with("checks={\"include\":[]}\nchecks_any=false\nchecks_expect=site\nsite_in_docs=false\nsite_deploy=false\ntrend_url=\n"), "{out}");
    let shown: Vec<_> = p.carried.iter().flat_map(|c| &c.shown).collect();
    assert!(shown.contains(&&("site".to_string(), "check")));

    let mut p = documented(&[("site", None)]);
    repo.write("book/intro.md", "x");
    repo.git(&["add", "."]);
    pull(&mut p, &repo, &store, Some(&site()), &["book/intro.md"]);
    assert!(p.carried.is_empty());
}

#[test]
fn a_site_with_a_hook_never_carries_and_holds_the_gates_with_it() {
    let repo = repo("carry-site-hook");
    let hook = Site {
        hook: true,
        ..site()
    };
    let mut p = documented(&[("site", None)]);
    let (_dir, store) = pass_all(&p, &repo, Some(&hook));
    pull(&mut p, &repo, &store, Some(&hook), &["docs/data.txt"]);
    assert!(p.carried.is_empty());
    assert!(p.outputs().contains("docs=true\n"));
}

#[test]
fn a_site_with_its_own_job_carries_alone() {
    let repo = repo("carry-site-alone");
    let mut p = checked(&[("site", None)]);
    let (_dir, store) = pass_all(&p, &repo, Some(&site()));
    pull(&mut p, &repo, &store, Some(&site()), &["docs/data.txt"]);
    assert!(p.carries("check:site"));
}

#[test]
fn the_escape_hatch_a_landing_a_push_and_an_unknown_diff_run_every_lane() {
    let repo = repo("carry-off");
    let mut p = lanes();
    let (_dir, store) = pass_all(&p, &repo, None);
    p.labels = vec![TEST_ALL_LABEL.into()];
    pull(&mut p, &repo, &store, None, &["docs/data.txt"]);
    assert!(p.carried.is_empty());

    let mut p = lanes();
    p.landing = true;
    pull(&mut p, &repo, &store, None, &["docs/data.txt"]);
    assert!(p.carried.is_empty());

    let mut p = lanes();
    let files = Files::read(&repo.root).unwrap();
    p.decide(&inputs(&files, &repo), None, Some(&store), &|_| {
        Some(String::new())
    });
    assert!(p.carried.is_empty());
    assert_eq!(p.recorded.len(), 3);
}

#[test]
fn a_lane_without_a_fingerprint_is_neither_carried_nor_recorded() {
    let repo = repo("carry-blind");
    let mut p = lanes();
    let (_dir, store) = pass_all(&p, &repo, None);
    p.rust = Some(crate::init::RustPlan {
        packages: vec!["core".into(), "ghost".into()],
        docs_packages: Vec::new(),
    });
    pull(&mut p, &repo, &store, None, &["docs/data.txt"]);
    assert_eq!(carried(&p), ["rust:core", "import:node"]);
    assert!(p.outputs().contains("packages=ghost\n"));
    assert_eq!(p.recorded.len(), 2);
}

#[test]
fn lanes_json_names_each_lane_and_the_results_a_run_must_leave() {
    let repo = repo("carry-json");
    let mut p = checked(&[("spec", Some("core"))]);
    p.shards = 2;
    let files = Files::read(&repo.root).unwrap();
    p.decide(&inputs(&files, &repo), None, None, &|_| Some(String::new()));
    let out = TempDir::new("plan-carry-json");
    p.write_lanes(&out).unwrap();
    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("lanes.json")).unwrap()).unwrap();
    let lane = |id: &str| {
        doc.as_array()
            .unwrap()
            .iter()
            .find(|l| l["lane"] == id)
            .unwrap()
    };
    assert_eq!(
        lane("rust:core")["records"],
        json!(["core#1/2", "core#2/2"])
    );
    assert_eq!(lane("rust:core")["kind"], "rust");
    assert_eq!(lane("import:node")["records"], json!(["node"]));
    assert_eq!(lane("check:spec-core")["kind"], "check");
    assert_eq!(lane("rust:core")["key"].as_str().unwrap().len(), 16);
    p.first_shard_only = true;
    assert_eq!(p.gate_records("core"), ["core#1/2"]);
}

#[test]
fn the_lane_flags_are_parsed() {
    let argv = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let flags = [
        "--soothfast-rev",
        "abc",
        "--rust-stable",
        "1.90",
        "--lanes",
        "out",
    ];
    let a = crate::plan::parse_args(&argv(&flags)).unwrap();
    assert_eq!((a.rev.as_str(), a.rust_stable.as_str()), ("abc", "1.90"));
    assert_eq!(a.lanes.as_deref(), Some(Path::new("out")));
    let none = crate::plan::parse_args(&argv(&[])).unwrap();
    assert!(none.rev.is_empty() && none.lanes.is_none());
}

#[test]
fn a_checked_out_action_among_the_changes_does_not_stop_a_carry() {
    let repo = repo("carry-scratch");
    let mut p = lanes();
    let (_dir, store) = pass_all(&p, &repo, None);
    repo.write(".soothfast-action/action.yml", "x");
    pull(
        &mut p,
        &repo,
        &store,
        None,
        &[".soothfast-action/action.yml"],
    );
    assert_eq!(carried(&p), ["rust:core", "rust:cli", "import:node"]);
}

fn empty_store(tag: &str) -> (TempDir, Store) {
    let dir = TempDir::new(tag);
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::at(dir.to_path_buf());
    (dir, store)
}

#[test]
fn a_touched_lane_carries_from_a_pull_request_pass_with_the_same_delta_only() {
    let repo = repo("carry-previous-head");
    let (_dir, store) = pass_all(&lanes(), &repo, None);
    repo.write("cli/src/lib.rs", "2");
    let changed = ["cli/src/lib.rs"];
    let mut p = lanes();
    pull(&mut p, &repo, &store, None, &changed);
    assert_eq!(carried(&p), ["rust:core", "import:node"]);

    pass_head(&repo, &store, &changed, "b1");
    let mut p = lanes();
    pull(&mut p, &repo, &store, None, &changed);
    assert_eq!(carried(&p), ["rust:core", "rust:cli", "import:node"]);
    assert_eq!(p.carried[1].commit, "feedface");
}

#[test]
fn a_base_that_moved_under_a_touched_input_runs_the_lane() {
    let repo = repo("carry-moved-base");
    let (_dir, store) = empty_store("carry-moved-base-store");
    repo.write("cli/src/lib.rs", "2");
    let changed = ["cli/src/lib.rs"];
    pass_head(&repo, &store, &changed, "b1");
    let mut p = lanes();
    pull_on(&mut p, &repo, &store, None, &changed, "b2");
    assert!(!p.carries("rust:cli"));
    pull_on(&mut p, &repo, &store, None, &changed, "b1");
    assert!(p.carries("rust:cli"));
}

#[test]
fn a_pull_request_pass_does_not_carry_to_a_head_that_changed_the_lane_again() {
    let repo = repo("carry-new-head");
    let (_dir, store) = empty_store("carry-new-head-store");
    repo.write("cli/src/lib.rs", "2");
    let changed = ["cli/src/lib.rs"];
    pass_head(&repo, &store, &changed, "b1");
    repo.write("cli/src/lib.rs", "3");
    let mut p = lanes();
    pull(&mut p, &repo, &store, None, &changed);
    assert!(!p.carries("rust:cli"));
}

#[test]
fn the_recorded_key_of_a_touched_lane_differs_from_the_default_branch_one() {
    let repo = repo("carry-recorded-key");
    let (_dir, store) = pass_all(&lanes(), &repo, None);
    repo.write("cli/src/lib.rs", "2");
    let mut p = lanes();
    pull(&mut p, &repo, &store, None, &["cli/src/lib.rs"]);
    let key = |p: &Plan, lane: &str| {
        p.recorded
            .iter()
            .find(|r| r.lane == lane)
            .unwrap()
            .key
            .clone()
    };
    let mut q = lanes();
    let files = Files::read(&repo.root).unwrap();
    q.decide(&inputs(&files, &repo), None, None, &|_| Some(String::new()));
    assert_ne!(key(&p, "rust:cli"), key(&q, "rust:cli"));
    assert_eq!(key(&p, "rust:core"), key(&q, "rust:core"));
}
