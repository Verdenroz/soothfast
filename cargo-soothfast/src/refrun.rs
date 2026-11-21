//! Where `report changelog` gets the measurement of the release it compares
//! against: HEAD's own baseline, a stored report, a run the gate recorded, or
//! a worktree build.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::buildstamp::BuildStamp;
use crate::config::{Config, Group};
use crate::headrun;
use crate::invoke::{self, CommonArgs};
use crate::runcache::{self, Runs};

/// The commit a reference measurement stands for, relative to HEAD.
#[derive(Debug, PartialEq)]
pub enum Anchor {
    /// HEAD is the merge-base: it has no commits the reference lacks.
    Head,
    /// The merge-base of HEAD and the reference; empty when git has none.
    At(String),
}

/// The merge-base of HEAD and `refname` in the repository at `dir`.
pub fn anchor(dir: &Path, refname: &str) -> Anchor {
    let sha = invoke::git_in(dir, &["merge-base", "HEAD", refname]).unwrap_or_default();
    match invoke::git_in(dir, &["rev-parse", "HEAD"]) {
        Ok(head) if head == sha => Anchor::Head,
        _ => Anchor::At(sha),
    }
}

/// The flags the release is measured under: `pkg`'s `gate` defaults, which
/// `measure` filled in for the baseline the measurement is compared with.
pub fn common(config: &Config, pkg: &str) -> Result<CommonArgs, String> {
    let mut common = CommonArgs {
        pkg: Some(pkg.to_string()),
        ..CommonArgs::default()
    };
    config.fill(
        &mut common,
        Group::Gate,
        std::slice::from_ref(&pkg.to_string()),
    )?;
    Ok(common)
}

/// The run the gate recorded for `sha`, kept under `key` as the report's
/// measurement of it. HEAD's bench binary names the host conditions the
/// recorded run was keyed on; without one nothing is read.
pub fn recorded_for(
    common: &CommonArgs,
    stamp: &BuildStamp,
    sha: &str,
    key: &str,
) -> Option<Value> {
    let exe = invoke::bench_executable(common, None, None)?;
    let env = invoke::host_env(&exe, common.backend.as_deref()).ok()?;
    recorded(&Runs::new(stamp, common, Some(env)), sha, key)
}

/// The run `runs` holds for `sha`, when it is that commit's callgrind
/// measurement of every item it names, kept under `key`. A run timed on
/// another backend, or one with an uncounted item, is not used.
fn recorded(runs: &Runs, sha: &str, key: &str) -> Option<Value> {
    let doc = runs.load(sha, sha)?;
    let items: BTreeSet<String> = invoke::run_from_items_value(&doc["items"])
        .items
        .into_keys()
        .collect();
    let usable = runs.gating_backend() == Some("callgrind")
        && doc["gating_backend"] == "callgrind"
        && headrun::readable_as_head(&doc)
        && headrun::counts_cover(&doc, &items);
    if usable {
        runcache::keep_report(key, &doc);
    }
    usable.then_some(doc)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{Anchor, anchor, common, headrun, recorded};
    use crate::buildstamp::BuildStamp;
    use crate::config::Config;
    use crate::invoke::{CommonArgs, HostEnv};
    use crate::runcache::{self, Runs};
    use crate::test::fixture::{TempDir, TempRepo};

    fn repo(name: &str) -> TempRepo {
        let repo = TempRepo::new(name);
        commit(&repo, "one");
        repo
    }

    fn commit(repo: &TempRepo, content: &str) -> String {
        repo.write("file", content);
        repo.git(&["add", "-A"]);
        repo.commit();
        repo.git(&["rev-parse", "HEAD"])
    }

    #[test]
    fn a_tag_on_head_anchors_at_head() {
        let repo = repo("refrun-at-head");
        repo.git(&["tag", "v1"]);
        assert_eq!(anchor(&repo.root, "v1"), Anchor::Head);
    }

    #[test]
    fn a_tag_behind_head_anchors_at_its_commit() {
        let repo = repo("refrun-behind");
        let tagged = repo.git(&["rev-parse", "HEAD"]);
        repo.git(&["tag", "v1"]);
        commit(&repo, "two");
        assert_eq!(anchor(&repo.root, "v1"), Anchor::At(tagged));
    }

    #[test]
    fn an_annotated_tag_anchors_at_the_commit_it_names() {
        let repo = repo("refrun-annotated");
        let tagged = repo.git(&["rev-parse", "HEAD"]);
        let identity = ["-c", "user.name=t", "-c", "user.email=t@t"];
        repo.git(&[&identity[..], &["tag", "-a", "v1", "-m", "release"]].concat());
        assert_ne!(repo.git(&["rev-parse", "v1"]), tagged);
        assert_eq!(anchor(&repo.root, "v1"), Anchor::Head);
        commit(&repo, "two");
        assert_eq!(anchor(&repo.root, "v1"), Anchor::At(tagged));
    }

    #[test]
    fn a_tag_off_the_branch_anchors_where_it_forked() {
        let repo = repo("refrun-forked");
        let fork = repo.git(&["rev-parse", "HEAD"]);
        repo.git(&["checkout", "-q", "-b", "side"]);
        commit(&repo, "side");
        repo.git(&["tag", "v1"]);
        repo.git(&["checkout", "-q", "-"]);
        assert_eq!(anchor(&repo.root, "v1"), Anchor::Head);
        commit(&repo, "two");
        assert_eq!(anchor(&repo.root, "v1"), Anchor::At(fork));
    }

    #[test]
    fn a_ref_git_cannot_resolve_anchors_nowhere() {
        let repo = repo("refrun-unknown");
        assert_eq!(anchor(&repo.root, "v9"), Anchor::At(String::new()));
    }

    fn stamp() -> BuildStamp {
        BuildStamp {
            rustc: "1.88.0".into(),
            codegen_units: "1".into(),
            profiles: "p".into(),
            rustflags: "f".into(),
        }
    }

    fn env(backend: &str) -> HostEnv {
        HostEnv {
            gating_backend: backend.into(),
            guest: None,
            callgrind_method: None,
            item_reuse: true,
        }
    }

    fn args() -> CommonArgs {
        CommonArgs {
            pkg: Some("demo".into()),
            ..CommonArgs::default()
        }
    }

    fn gate_run(backend: &str, items: serde_json::Value) -> Value {
        let mut doc = serde_json::json!({
            "version": 1,
            "gating_backend": backend,
            "items": items,
        });
        headrun::stamp(&mut doc);
        doc
    }

    fn counted() -> serde_json::Value {
        serde_json::json!({
            "demo::a": { "fingerprint": "x", "callgrind": { "ir": 10 } },
            "demo::b": { "fingerprint": "y", "callgrind": { "ir": 20 } },
        })
    }

    /// `doc` stored as the gate stores a commit's run, read back through
    /// `recorded` under `backend`'s environment, with the report key it was
    /// kept under.
    fn read_back(sha: &str, backend: &str, doc: &Value) -> (Option<Value>, Option<Value>) {
        let (stamp, args) = (stamp(), args());
        let runs = Runs::new(&stamp, &args, Some(env(backend)));
        runs.store(sha, doc);
        let report = runcache::report_key(sha, &args, &stamp);
        let got = recorded(&runs, sha, &report);
        let kept = runcache::stored_report(&report);
        runcache::forget(&runcache::key(sha, &stamp, &args, &env(backend)));
        runcache::forget(&report);
        (got, kept)
    }

    #[test]
    fn a_callgrind_run_the_gate_recorded_is_used_and_kept_as_the_reports() {
        let doc = gate_run("callgrind", counted());
        let (got, kept) = read_back("test-refrun-recorded", "callgrind", &doc);
        assert_eq!(got.expect("recorded run")["items"], doc["items"]);
        assert_eq!(kept.expect("kept report")["items"], doc["items"]);
    }

    #[test]
    fn a_run_with_an_uncounted_item_is_not_used() {
        let mut items = counted();
        items["demo::b"] = serde_json::json!({ "fingerprint": "y" });
        let doc = gate_run("callgrind", items);
        assert_eq!(
            read_back("test-refrun-uncounted", "callgrind", &doc),
            (None, None)
        );
    }

    #[test]
    fn a_run_timed_on_another_backend_is_not_used() {
        let doc = gate_run("walltime", counted());
        assert_eq!(
            read_back("test-refrun-timed", "walltime", &doc),
            (None, None)
        );
        assert_eq!(
            read_back("test-refrun-mixed", "callgrind", &doc),
            (None, None)
        );
    }

    #[test]
    fn a_run_stored_without_its_verdicts_is_not_used() {
        let doc = serde_json::json!({
            "version": 1, "gating_backend": "callgrind", "items": counted()
        });
        assert_eq!(
            read_back("test-refrun-unstamped", "callgrind", &doc),
            (None, None)
        );
    }

    fn configured(name: &str, text: &str) -> Config {
        let dir = TempDir::new(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("soothfast.toml"), text).unwrap();
        Config::load(&dir).unwrap()
    }

    const TWO_GROUPS: &str = "[package.demo.docs]\nfeatures = \"full\"\n\n\
        [package.demo.gate]\nfeatures = \"bench\"\ntarget = \"t\"\n";

    #[test]
    fn the_release_is_measured_under_the_gate_groups_features() {
        let cfg = configured("refrun-gate-group", TWO_GROUPS);
        let measured = common(&cfg, "demo").unwrap();
        assert_eq!(measured.features.as_deref(), Some("bench"));
        assert_eq!(measured.target.as_deref(), Some("t"));
        assert_eq!(measured.pkg.as_deref(), Some("demo"));
    }

    #[test]
    fn the_report_key_follows_the_gate_groups_features_and_target() {
        let cfg = configured("refrun-gate-key", TWO_GROUPS);
        let measured = common(&cfg, "demo").unwrap();
        let key = |common: &CommonArgs| runcache::report_key("aaa", common, &stamp());
        let with = |features: &str, target: &str| CommonArgs {
            features: Some(features.into()),
            target: Some(target.into()),
            ..args()
        };
        assert_eq!(key(&measured), key(&with("bench", "t")));
        assert_ne!(key(&measured), key(&with("full", "t")));
        assert_ne!(key(&measured), key(&with("bench", "other")));
    }

    #[test]
    fn the_gates_codegen_units_pin_reaches_the_release_build() {
        let cfg = configured("refrun-codegen", "[gate]\ncodegen-units = 4\n");
        let measured = common(&cfg, "demo").unwrap();
        assert_eq!(measured.codegen_units.as_deref(), Some("4"));
    }
}
