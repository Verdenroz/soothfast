//! `action/fork-comment.sh` renders a fork's verdict from its run's results
//! with a stub `gh`, outside any repository. Everything the fork uploaded is
//! hostile input.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const GH: &str = r#"#!/bin/sh
case "$*" in
*/pulls/*) [ -n "${STUB_PULL-}" ] || exit 1; printf '%s\n' "$STUB_PULL" ;;
*"-X POST"* | *"-X PATCH"*)
  for a; do case $a in body=@*) cp "${a#body=@}" "$STUB_POSTED" ;; esac; done ;;
esac
"#;

struct Outcome {
    code: i32,
    posted: Option<String>,
    stdout: String,
}

struct Case {
    root: PathBuf,
    pull: Option<String>,
    head_sha: String,
    conclusion: String,
}

impl Case {
    fn new(tag: &str) -> Case {
        let root =
            std::env::temp_dir().join(format!("soothfast-fork-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for dir in ["bin", "work", "tmp", "artifact", "results"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("bin/gh"), GH).unwrap();
        Command::new("chmod")
            .arg("+x")
            .arg(root.join("bin/gh"))
            .status()
            .unwrap();
        let case = Case {
            root,
            pull: Some("open\tabc123\tfork/repo\tfeature".into()),
            head_sha: "abc123".into(),
            conclusion: "success".into(),
        };
        case.upload("pr-number", "7\n");
        case
    }

    fn upload(&self, file: &str, text: &str) {
        fs::write(self.root.join("artifact").join(file), text).unwrap();
    }

    fn result(&self, name: &str, verdict: &str, tail: &str) {
        let dir = self.root.join("results/soothfast-result-core-attempt-1");
        fs::create_dir_all(&dir).unwrap();
        let rec = serde_json::json!({
            "name": name, "kind": "rust", "verdict": verdict, "tail": tail, "seconds": 3,
        });
        fs::write(dir.join(format!("{name}.json")), rec.to_string()).unwrap();
    }

    fn results(&self) -> PathBuf {
        self.root.join("results")
    }

    fn run(&self) -> Outcome {
        let posted = self.root.join("posted.md");
        let results = self.results();
        let results = if results.exists() {
            results
        } else {
            self.root.join("none")
        };
        let out = Command::new("bash")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../action/fork-comment.sh"))
            .current_dir(self.root.join("work"))
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("CLI", env!("CARGO_BIN_EXE_cargo-soothfast"))
            .env("ARTIFACT_DIR", self.root.join("artifact"))
            .env("RESULTS", results)
            .env("RUNNER_TEMP", self.root.join("tmp"))
            .env("HEAD_SHA", &self.head_sha)
            .env("HEAD_REPO", "fork/repo")
            .env("HEAD_BRANCH", "feature")
            .env("RUN_ATTEMPT", "1")
            .env("RUN_CONCLUSION", &self.conclusion)
            .env("RUN_URL", "https://github.com/base/repo/actions/runs/9")
            .env("GH_TOKEN", "t")
            .env("GITHUB_REPOSITORY", "base/repo")
            .env("STUB_POSTED", &posted)
            .envs(self.pull.iter().map(|p| ("STUB_PULL", p)))
            .output()
            .unwrap();
        let outcome = Outcome {
            code: out.status.code().unwrap(),
            posted: fs::read_to_string(&posted).ok(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        };
        println!(
            "| {} | {} | {} |",
            self.root.file_name().unwrap().to_string_lossy(),
            outcome.code,
            outcome
                .posted
                .as_deref()
                .map_or("nothing", |b| b.lines().nth(3).unwrap_or(""))
        );
        outcome
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_valid_run_posts_the_rendered_verdict_under_trusted_lines() {
    let case = Case::new("valid");
    case.result("core", "pass", "ok");
    case.upload("expect_rust", "core\n");
    let out = case.run();
    assert_eq!(out.code, 0);
    let body = out.posted.unwrap();
    assert!(
        body.starts_with("<!-- soothfast-verdict -->\nVerdict of [the fork's run](https://github.com/base/repo/actions/runs/9), which concluded `success`.\n\n## soothfast verdict\n1 projects: 0 regressed, 0 errored, 1 pass\n"),
        "{body}"
    );
    assert!(!body.contains("did not succeed"));
}

#[test]
fn an_invalid_pull_request_number_posts_nothing() {
    for number in ["abc", "0", "-1", "01", "1 2", "12345678901", "", "7; id"] {
        let case = Case::new("number");
        case.result("core", "pass", "ok");
        case.upload("pr-number", &format!("{number}\n"));
        let out = case.run();
        assert_eq!((out.code, out.posted), (0, None), "{number:?}");
        assert!(out.stdout.contains("no comment posted"), "{number:?}");
    }
}

#[test]
fn a_missing_number_posts_nothing() {
    let case = Case::new("nonumber");
    fs::remove_file(case.root.join("artifact/pr-number")).unwrap();
    assert_eq!(case.run().posted, None);
}

#[test]
fn a_number_that_does_not_belong_to_the_run_posts_nothing() {
    let cases = [
        ("unreadable", None),
        ("closed", Some("closed\tabc123\tfork/repo\tfeature")),
        ("moved", Some("open\tdef456\tfork/repo\tfeature")),
        ("same repository", Some("open\tabc123\tbase/repo\tfeature")),
        ("other fork", Some("open\tabc123\tother/repo\tfeature")),
        ("other branch", Some("open\tabc123\tfork/repo\tmain")),
    ];
    for (tag, pull) in cases {
        let mut case = Case::new("owner");
        case.result("core", "pass", "ok");
        case.pull = pull.map(String::from);
        let out = case.run();
        assert_eq!((out.code, out.posted), (0, None), "{tag}");
    }
}

#[test]
fn an_expectation_with_shell_metacharacters_is_dropped_unrun() {
    let case = Case::new("meta");
    case.result("core", "pass", "ok");
    let marker = case.root.join("pwned");
    case.upload(
        "expect_rust",
        &format!(
            "core $(touch {0}) `touch {0}` ;touch {0} a|b *.json ok\n",
            marker.display()
        ),
    );
    case.upload("expect_shards", "1/2 $(id) x/y 1/2/3\n");
    case.upload("landing", "true; id\n");
    let out = case.run();
    assert_eq!(out.code, 0);
    assert!(!marker.exists());
    let body = out.posted.unwrap();
    assert!(body.contains("### ok#1/2\nerrored | rust"), "{body}");
    assert!(!body.contains("touch") && !body.contains("$(id)"), "{body}");
    assert!(!body.contains("No lane ran"), "{body}");
    assert!(out.stdout.contains("dropped"), "{}", out.stdout);
}

#[test]
fn an_oversized_expectation_list_is_cut_at_the_limit() {
    let case = Case::new("many");
    case.result("core", "pass", "ok");
    let names: Vec<String> = (0..300).map(|i| format!("n{i}")).collect();
    case.upload("expect_rust", &names.join(" "));
    let out = case.run();
    assert_eq!(out.code, 0);
    let body = out.posted.unwrap();
    assert!(
        body.contains("### n255\n") && !body.contains("### n256\n"),
        "{body}"
    );
    assert!(out.stdout.contains("dropped 44"), "{}", out.stdout);
}

#[test]
fn missing_results_are_reported_by_the_trusted_script() {
    let case = Case::new("noresults");
    fs::remove_dir_all(case.results()).unwrap();
    case.upload("expect_rust", "core\n");
    let out = case.run();
    assert_eq!(out.code, 0);
    let body = out.posted.unwrap();
    assert!(
        body.contains("No results were found for this run"),
        "{body}"
    );
    assert!(!body.contains("###"), "{body}");
}

#[test]
fn oversized_results_are_not_rendered() {
    let case = Case::new("large");
    case.result("core", "pass", "ok");
    fs::write(
        case.results()
            .join("soothfast-result-core-attempt-1/big.json"),
        vec![b' '; 21 << 20],
    )
    .unwrap();
    let body = case.run().posted.unwrap();
    assert!(body.contains("too large to render"), "{body}");
}

#[test]
fn a_hostile_file_name_and_fence_breaking_content_stay_inert() {
    let case = Case::new("hostile");
    case.result(
        "core",
        "errored",
        "```\n## forged heading\n```\n[x](http://evil)",
    );
    let dir = case.results().join("soothfast-result-core-attempt-1");
    fs::write(dir.join("a\n### injected heading.json"), "{}").unwrap();
    fs::write(dir.join("$(id).json"), "{}").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", dir.join("link.json")).unwrap();
    let out = case.run();
    assert_eq!(out.code, 0);
    let body = out.posted.unwrap();
    assert!(!body.contains("injected"), "{body}");
    assert!(!body.contains("root:"), "{body}");
    assert!(
        body.contains("` ` `\n## forged heading\n` ` `\n[x](http://evil)\n```\n"),
        "{body}"
    );
    let headings = body.lines().filter(|l| l.starts_with("###")).count();
    assert_eq!(headings, 1, "{body}");
}

#[test]
fn a_failed_run_with_passing_results_says_so() {
    let mut case = Case::new("failure");
    case.result("core", "pass", "ok");
    case.conclusion = "failure".into();
    let body = case.run().posted.unwrap();
    assert!(body.contains("which concluded `failure`."), "{body}");
    assert!(body.contains("The run did not succeed"), "{body}");
}

#[test]
fn results_of_a_later_attempt_are_left_out() {
    let case = Case::new("attempt");
    case.result("core", "pass", "ok");
    let later = case.results().join("soothfast-result-late-attempt-2");
    fs::create_dir_all(&later).unwrap();
    fs::write(
        later.join("late.json"),
        r#"{"name":"late","kind":"rust","verdict":"pass","tail":"","seconds":1}"#,
    )
    .unwrap();
    let body = case.run().posted.unwrap();
    assert!(
        body.contains("### core") && !body.contains("### late"),
        "{body}"
    );
}
