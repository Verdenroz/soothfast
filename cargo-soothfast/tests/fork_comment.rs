//! `action/fork-comment.sh` renders a fork's verdict from its run's results
//! with a stub `gh`, outside any repository. Everything the fork uploaded is
//! hostile input. A stub `curl` stands in for the OIDC endpoint and the broker.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const GH: &str = r#"#!/bin/sh
case "$*" in
*"--jq .head.sha") printf '%s\n' "${STUB_HEAD_NOW-$(printf '%s' "${STUB_PULL-}" | cut -f 2)}" ;;
*/pulls/*) [ -n "${STUB_PULL-}" ] || exit 1; printf '%s\n' "$STUB_PULL" ;;
*"-X POST"* | *"-X PATCH"*)
  for a; do case $a in body=@*) cp "${a#body=@}" "$STUB_POSTED" ;; esac; done ;;
esac
"#;

const CURL: &str = r#"#!/bin/sh
case "$*" in
*audience=soothfast-bot*) printf '{"value":"jwt"}' ;;
*/comment*)
  [ "$STUB_BROKER_STATUS" != unreachable ] || exit 7
  prev=
  for a; do [ "$prev" != --data-binary ] || printf '%s' "$a" >"$STUB_PAYLOAD"; prev=$a; done
  printf '%s\n%s' "$STUB_BROKER_BODY" "$STUB_BROKER_STATUS" ;;
esac
"#;

struct Outcome {
    code: i32,
    /// The body the job token posted.
    posted: Option<String>,
    /// The JSON the broker was sent.
    payload: Option<String>,
    stdout: String,
}

struct Case {
    root: PathBuf,
    pull: Option<String>,
    head_sha: String,
    conclusion: String,
    /// The head `gh` reports when the fallback re-reads it, when it has moved.
    head_now: Option<String>,
    /// The broker's HTTP status (or `unreachable`) and body. `None` is a job
    /// with no OIDC token.
    broker: Option<(&'static str, &'static str)>,
}

impl Case {
    fn new(tag: &str) -> Case {
        let root =
            std::env::temp_dir().join(format!("soothfast-fork-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for dir in ["bin", "work", "tmp", "artifact", "results"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        for (tool, script) in [("gh", GH), ("curl", CURL)] {
            fs::write(root.join("bin").join(tool), script).unwrap();
            Command::new("chmod")
                .arg("+x")
                .arg(root.join("bin").join(tool))
                .status()
                .unwrap();
        }
        let case = Case {
            root,
            pull: Some("open\tabc123\tfork/repo\tfeature".into()),
            head_sha: "abc123".into(),
            conclusion: "success".into(),
            head_now: None,
            broker: None,
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
        let out = self.script("fork-comment.sh", &[("RUN_ID", "9")]);
        println!(
            "| {} | {} | {} |",
            self.root.file_name().unwrap().to_string_lossy(),
            out.code,
            out.posted
                .as_deref()
                .map_or("nothing", |b| b.lines().nth(3).unwrap_or(""))
        );
        out
    }

    /// Runs `action/NAME` the way `comment.yml` runs `fork-comment.sh`, with
    /// `more` set on top.
    fn script(&self, name: &str, more: &[(&str, &str)]) -> Outcome {
        let posted = self.root.join("posted.md");
        let payload = self.root.join("payload.json");
        let mut cmd = Command::new("bash");
        cmd.arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../action")
                .join(name),
        )
        .current_dir(self.root.join("work"))
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
        )
        .env("CLI", env!("CARGO_BIN_EXE_cargo-soothfast"))
        .env("ARTIFACT_DIR", self.root.join("artifact"))
        .env("RESULTS", self.results())
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
        .env("STUB_PAYLOAD", &payload)
        .envs(self.pull.iter().map(|p| ("STUB_PULL", p)))
        .envs(self.head_now.iter().map(|h| ("STUB_HEAD_NOW", h)))
        .envs(more.iter().copied());
        if let Some((status, body)) = self.broker {
            cmd.env(
                "ACTIONS_ID_TOKEN_REQUEST_URL",
                "https://oidc.invalid/token?v=1",
            )
            .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "request")
            .env("BROKER", "https://broker.invalid")
            .env("STUB_BROKER_STATUS", status)
            .env("STUB_BROKER_BODY", body);
        }
        let out = cmd.output().unwrap();
        Outcome {
            code: out.status.code().unwrap(),
            posted: fs::read_to_string(&posted).ok(),
            payload: fs::read_to_string(&payload).ok(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        }
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
    case.upload("landing", "true\n");
    let out = case.run();
    assert_eq!(out.code, 0);
    assert!(!marker.exists());
    let body = out.posted.unwrap();
    assert!(body.contains("### `ok#1/2`\nerrored | rust"), "{body}");
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
        body.contains("### `n255`\n") && !body.contains("### `n256`\n"),
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
        body.contains("### `core`") && !body.contains("### `late`"),
        "{body}"
    );
}

#[test]
fn a_lone_artifact_extracted_without_its_directory_is_rendered() {
    let case = Case::new("flat");
    fs::write(
        case.results().join("core.json"),
        r#"{"name":"core","kind":"rust","verdict":"pass","tail":"ok","seconds":1}"#,
    )
    .unwrap();
    let body = case.run().posted.unwrap();
    assert!(body.contains("### `core`\npass | rust | 1s"), "{body}");
}

#[test]
fn no_field_of_a_result_is_markup_outside_a_fence() {
    let case = Case::new("fields");
    let dir = case.results().join("soothfast-result-core-attempt-1");
    fs::create_dir_all(&dir).unwrap();
    let hostile = [
        r##"{"name":"a","kind":"rust","verdict":"pass","seconds":1,"carried":"\n```","tail":"[x](http://evil) @someone"}"##,
        r##"{"name":"www.evil.example","kind":"https://evil.example","verdict":"pass","seconds":1,"carried":"@harvey","tail":""}"##,
        r##"{"name":"GH-1\n# forged","kind":"a/b#2","verdict":"regressed","seconds":1e300,"carried":"#2","tail":"~~~\n````"}"##,
        r##"{"name":"b#<b>","kind":"|\n---|---","verdict":"flaky","seconds":1,"flaky":true,"unevaluated":true,
            "reuse":{"covered":18446744073709551615,"total":18446744073709551615,"measuring":1},"output":"x"}"##,
        r##"{"name":"b#2","kind":"test","verdict":"flaky","wall_s":1,
            "reuse":{"covered":18446744073709551615,"total":18446744073709551615,"measuring":1},"output":"x"}"##,
    ];
    for (i, record) in hostile.iter().enumerate() {
        fs::write(dir.join(format!("{i}.json")), record).unwrap();
    }
    let out = case.run();
    assert_eq!(out.code, 0);
    let body = out.posted.unwrap();
    let status = |l: &str| {
        ["pass", "errored", "regressed", "nothing-to-compare"]
            .iter()
            .any(|v| l.starts_with(v))
            && l.chars()
                .all(|c| c.is_ascii_alphanumeric() || " ()|/,-?".contains(c))
    };
    let mut fenced = false;
    for line in body.lines().skip(3) {
        if line == "```" {
            fenced = !fenced;
        } else if !fenced {
            let heading =
                line.starts_with("### `") && line.ends_with('`') && line.matches('`').count() == 2;
            let summary = line == "## soothfast verdict" || line.contains(" projects: ");
            assert!(
                line.is_empty() || heading || summary || status(line),
                "{line:?} in\n{body}"
            );
        }
    }
    assert!(!fenced, "{body}");
    assert_eq!(body.matches("\n### ").count(), 4, "{body}");
}

#[test]
fn a_shard_result_is_read_under_the_name_the_gate_writes() {
    let case = Case::new("shard");
    let dir = case
        .results()
        .join("soothfast-result-rust-shard-1-of-2-attempt-1");
    fs::create_dir_all(&dir).unwrap();
    for shard in ["1", "2"] {
        fs::write(
            dir.join(format!("core#{shard}-2.json")),
            format!(
                r##"{{"name":"core#{shard}/2","kind":"rust","verdict":"pass","tail":"ok","seconds":1}}"##
            ),
        )
        .unwrap();
    }
    case.upload("expect_rust", "core\n");
    case.upload("expect_shards", "1/2 2/2\n");
    let out = case.run();
    assert_eq!(out.code, 0);
    let body = out.posted.unwrap();
    assert!(
        body.contains("1 projects: 0 regressed, 0 errored, 1 pass"),
        "{body}"
    );
}

const POSTED_BY_BOT: &str =
    r#"{"comment_url":"https://github.com/base/repo/pull/7#c","app_slug":"soothfast-bot"}"#;

#[test]
fn the_broker_posts_the_verdict_with_the_head_it_is_for() {
    let mut case = Case::new("broker");
    case.result("core", "pass", "ok");
    case.broker = Some(("200", POSTED_BY_BOT));
    let out = case.run();
    assert_eq!((out.code, out.posted), (0, None));
    assert!(
        out.stdout.contains("commented as soothfast-bot"),
        "{}",
        out.stdout
    );
    let sent: serde_json::Value = serde_json::from_str(&out.payload.unwrap()).unwrap();
    assert_eq!(sent["pull_request"], 7);
    assert_eq!(sent["marker"], "<!-- soothfast-verdict -->");
    assert_eq!(sent["head_sha"], "abc123");
    assert_eq!(sent["run_id"], 9);
    let body = sent["body"].as_str().unwrap();
    assert!(
        body.starts_with("Verdict of [the fork's run](") && body.contains("### `core`"),
        "{body}"
    );
}

const NOTICE: &str = "::notice::the broker did not verify the run";

#[test]
fn an_unverified_run_is_noticed_and_a_verified_one_is_not() {
    let replies = [
        (r#"{"app_slug":"soothfast-bot","run_verified":false}"#, true),
        (r#"{"app_slug":"soothfast-bot","run_verified":true}"#, false),
        (r#"{"app_slug":"soothfast-bot"}"#, false),
    ];
    for (reply, noticed) in replies {
        let mut case = Case::new("verified");
        case.result("core", "pass", "ok");
        case.broker = Some(("200", reply));
        let out = case.run();
        assert_eq!(out.code, 0, "{reply}");
        assert_eq!(out.stdout.contains(NOTICE), noticed, "{}", out.stdout);
    }
}

#[test]
fn a_broker_that_does_not_post_leaves_it_to_the_job_token() {
    let refusals = [
        Some((
            "403",
            r#"{"reason":"/comment serves pull_request runs only"}"#,
        )),
        Some(("unreachable", "")),
        Some(("502", "<html>bad gateway</html>")),
        None,
    ];
    for broker in refusals {
        let mut case = Case::new("refused");
        case.result("core", "pass", "ok");
        case.broker = broker;
        let out = case.run();
        assert_eq!(out.code, 0, "{broker:?}");
        let body = out
            .posted
            .unwrap_or_else(|| panic!("{broker:?}: {}", out.stdout));
        assert!(body.contains("### `core`"), "{broker:?}");
    }
}

#[test]
fn a_head_that_moved_is_posted_by_neither_the_broker_nor_the_job_token() {
    let brokers = [
        Some((
            "403",
            r#"{"reason":"pull request 7 head moved to def4567"}"#,
        )),
        Some(("unreachable", "")),
        None,
    ];
    for broker in brokers {
        let mut case = Case::new("moved");
        case.result("core", "pass", "ok");
        case.broker = broker;
        case.head_now = Some("def456".into());
        let out = case.run();
        assert_eq!((out.code, out.posted), (0, None), "{broker:?}");
        assert!(out.stdout.contains("has moved past"), "{}", out.stdout);
    }
}

#[test]
fn a_pull_request_job_sends_the_broker_no_head() {
    let mut case = Case::new("samerepo");
    case.broker = Some(("200", POSTED_BY_BOT));
    let body = case.root.join("body.md");
    fs::write(&body, "hello\n").unwrap();
    let env = [
        ("PR_NUMBER", "7"),
        ("MARKER", "<!-- soothfast-verdict -->"),
        ("BODY_FILE", body.to_str().unwrap()),
        ("HEAD_SHA", ""),
    ];
    let out = case.script("comment.sh", &env);
    assert_eq!((out.code, out.posted), (0, None));
    assert_eq!(
        out.payload.unwrap(),
        "{\n  \"pull_request\": 7,\n  \"marker\": \"<!-- soothfast-verdict -->\",\n  \"body\": \"hello\\n\"\n}"
    );
}
