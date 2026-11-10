//! `ci verdict`: folds every job's record under the results directory into
//! the pull request comment body and the run's verdict. Posting the body is
//! left to the caller.

use std::path::PathBuf;

use super::env::CiEnv;
use crate::verdict::{expected, merge_dir};

const BODY_FILE: &str = "verdict-comment.md";

struct Args {
    expect: Vec<(String, String)>,
    accepted: Option<String>,
    out: Option<PathBuf>,
}

/// Entry point for `ci verdict`: `[--expect NAME:KIND]... [--accepted LABEL]
/// [--out FILE]`. Exits 1 when a job errored, recorded nothing it was
/// expected to, or regressed without a waiver.
pub fn run(env: &CiEnv, args: &[String]) -> i32 {
    let result = parse(args).and_then(|args| {
        let dir = env
            .results
            .as_ref()
            .ok_or("verdict needs a results directory")?;
        let out = args.out.unwrap_or_else(|| dir.join(BODY_FILE));
        merge_dir(dir, &out, args.accepted.as_deref(), &args.expect)
    });
    match result {
        Ok(code) => code,
        Err(e) => crate::arg_err(&e),
    }
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args {
        expect: Vec::new(),
        accepted: None,
        out: None,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "--expect" => parsed.expect.push(expected(&value()?)?),
            "--accepted" => parsed.accepted = Some(value()?),
            "--out" => parsed.out = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown verdict argument {other:?}")),
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::env::Flags;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn env(results: Option<&Path>) -> CiEnv {
        let flags = Flags {
            results: results.map(Path::to_path_buf),
            ..Flags::default()
        };
        CiEnv::parse(&BTreeMap::new(), None, &flags)
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("soothfast-ci-v-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put(dir: &Path, name: &str, kind: &str, verdict: &str, tail: &str) {
        let rec = serde_json::json!({
            "name": name, "kind": kind, "verdict": verdict, "tail": tail, "seconds": 3,
        });
        fs::write(dir.join(format!("{name}.json")), rec.to_string()).unwrap();
    }

    fn body(dir: &Path) -> String {
        fs::read_to_string(dir.join(BODY_FILE)).unwrap()
    }

    #[test]
    fn the_body_is_written_under_the_results_directory_by_default() {
        let dir = scratch("body");
        put(&dir, "core", "rust", "pass", "ok");
        assert_eq!(run(&env(Some(&dir)), &[]), 0);
        assert!(body(&dir).contains("### core\npass | rust | 3s"));
        let custom = dir.join("custom.md");
        let out = custom.to_str().unwrap();
        assert_eq!(run(&env(Some(&dir)), &argv(&["--out", out])), 0);
        assert!(custom.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_expected_job_with_no_record_fails_the_verdict() {
        let dir = scratch("expect");
        put(&dir, "core", "rust", "pass", "ok");
        let present = argv(&["--expect", "core:rust"]);
        assert_eq!(run(&env(Some(&dir)), &present), 0);
        let missing = argv(&["--expect", "core:rust", "--expect", "docs:docs"]);
        assert_eq!(run(&env(Some(&dir)), &missing), 1);
        assert!(body(&dir).contains("### docs\nerrored | docs"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn accepted_waives_a_regression_but_not_an_error_or_a_check() {
        let accept = argv(&["--accepted", "perf-ok"]);
        let dir = scratch("waive");
        put(&dir, "core", "rust", "regressed", "slower");
        assert_eq!(run(&env(Some(&dir)), &[]), 1);
        assert_eq!(run(&env(Some(&dir)), &accept), 0);
        assert!(body(&dir).contains("accepted by the `perf-ok` label"));
        put(&dir, "gate", "import", "errored", "broke");
        assert_eq!(run(&env(Some(&dir)), &accept), 1);
        fs::remove_dir_all(&dir).unwrap();

        let dir = scratch("check");
        put(&dir, "core", "rust", "regressed", "slower");
        put(&dir, "spec", "check", "errored", "drift");
        assert_eq!(run(&env(Some(&dir)), &accept), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn job_output_cannot_close_the_comment_fence() {
        let dir = scratch("fence");
        put(&dir, "core", "rust", "pass", "x\n```\n@everyone");
        run(&env(Some(&dir)), &[]);
        assert_eq!(body(&dir).matches("```").count(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bad_arguments_and_a_missing_results_directory_are_usage_errors() {
        let dir = scratch("usage");
        assert_eq!(run(&env(Some(&dir)), &argv(&["--nope"])), 2);
        assert_eq!(run(&env(Some(&dir)), &argv(&["--expect", "core"])), 2);
        assert_eq!(run(&env(Some(&dir)), &argv(&["--out"])), 2);
        assert_eq!(run(&env(None), &[]), 2);
        fs::remove_dir_all(dir).unwrap();
    }
}
